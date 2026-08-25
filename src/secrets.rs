//! Secret detection for the write path.
//!
//! `remember` refuses to persist a memory whose body or evidence looks like a
//! credential, so a secret can never enter the local store and therefore can
//! never leak later through `admin export` -> `.limpet/memory.jsonl` -> git.
//!
//! Detection is deliberately high-precision (provider-specific prefixes with
//! length and charset checks) to avoid false positives on ordinary prose like
//! "the endpoint returns a bearer token". No regex dependency: the whole scan
//! is a token walk plus a couple of substring checks.

/// The first credential detected in `text`, as a human-readable label, or
/// `None` if nothing matched.
pub fn detect(text: &str) -> Option<&'static str> {
    // Multi-line block markers, checked on the whole text.
    if text.contains("-----BEGIN") && text.contains("PRIVATE KEY-----") {
        return Some("private key block");
    }

    // Everything else is a self-contained token: split on characters that
    // never appear inside these credentials and inspect each candidate.
    // '=' and ':' matter most: `AWS_KEY=AKIA...` and `token: ghp_...` are
    // the standard .env/YAML shapes and slipped through unsplit
    // (audit 2026-07).
    for tok in text.split(is_boundary) {
        if let Some(label) = classify_candidate(tok) {
            return Some(label);
        }
        // A period ends a sentence, but it also joins a JWT's three segments,
        // so it cannot sit in `is_boundary`: splitting there would destroy the
        // one credential shape that is built out of dots. Instead the whole
        // token is classified first (JWT intact), and only if that finds
        // nothing is the token re-split on periods. That catches a credential
        // glued to the next sentence with no space ("...EXAMPLE.Then rotate"),
        // which the trim above cannot reach (audit 2026-08).
        if tok.contains('.') {
            for part in tok.split('.') {
                if let Some(label) = classify_candidate(part) {
                    return Some(label);
                }
            }
        }
    }
    None
}

/// Characters that end a candidate token: everything that is not a letter,
/// not a digit, and not one of the three characters a credential is built out
/// of.
///
/// The rule is a CLASS, not a list. The 2026-07 set enumerated nineteen ASCII
/// characters and the 2026-08 first cut added seven more, which still left
/// every Unicode punctuation class outside the rule and `&` inside it:
/// `\u{201c}AKIA...\u{201d}` (the substitution macOS, Slack and Notes make
/// automatically around a pasted quoted key) and `key=<cred>&next=1` both
/// reached the classifier with the neighbouring text still glued on, missing
/// the `n == 20` gate exactly like the plain period used to.
///
/// The three exceptions are the credential body characters. `-` and `_` are
/// embedded in Slack, OpenAI, Stripe and GitHub PAT bodies, and `.` joins a
/// JWT's three segments (it is peeled at the token EDGES by
/// `trim_edge_punctuation` and re-split inside `detect` instead), so splitting
/// on any of the three would break detection instead of widening it.
fn is_boundary(c: char) -> bool {
    !c.is_alphanumeric() && !matches!(c, '-' | '_' | '.')
}

/// Peel leading and trailing punctuation off a candidate token, so a key
/// wrapped in `**bold**`, `(parens)`, `\u{201c}smart quotes\u{201d}` or
/// trailing `...` reaches the classifier as the bare token does.
///
/// `-` and `_` survive this pass. They are credential body characters, and the
/// Google gate is an EXACT `n == 39` over a url-safe base64 charset that
/// CONTAINS both, so peeling them here would drop a real 39-char `AIza` key
/// ending in `-` to 38 bytes and miss the gate. A boundary rule that shortens
/// a token through an exact gate deletes detection just as surely as one that
/// lengthens it through.
///
/// A trailing `.` IS peeled here, unlike in `is_boundary`: a JWT's dots are
/// interior, so the sentence period that follows one comes off the edge
/// without touching the three segments.
///
/// This is a BOUNDARY rule only. The classifier's own length and charset gates
/// are untouched, so a trimmed token must satisfy exactly the same test the
/// bare token does: `AKIA` + 16 alnum + `X` is 21 bytes trimmed and is still
/// not a key (I-A2).
fn trim_edge_punctuation(tok: &str) -> &str {
    tok.trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
}

/// True when a body character (`-` or `_`) sits BETWEEN two of the candidate's
/// own alphanumerics, i.e. is spelled as a word SEPARATOR rather than wrapped
/// around an opaque blob as emphasis.
///
/// A credential may legitimately END in a body character (`AIza...qrstu_` is a
/// real 39-byte Google key) and may carry sentence punctuation after it
/// (`AIza...qrstu_.`), so neither EDGE says anything. One sitting between the
/// first and the last alphanumeric does: that is how
/// `sk_test_refund_path_covered_here` and `sk-color-surface-elevated-default`
/// are spelled, and it is the difference between markdown emphasis around an
/// opaque blob and an identifier being quoted in a sentence.
///
/// BOTH body characters count, not just the one being peeled. The emphasis
/// character and the identifier's separator do not have to match:
/// `--rk_live_reload_watcher_thread--` and `__sk-color-surface-elevated-default__`
/// are the same false positive wearing the other delimiter.
///
/// This is an INVARIANT of the whole candidate, not a property of one peel.
/// Every contraction below strips NON-alphanumeric characters off the EDGES
/// only, so the first alphanumeric, the last alphanumeric and every byte
/// between them are the same at every step. `classify_candidate` therefore
/// evaluates it once and threads the answer through, which is also what keeps
/// the peel linear: re-deriving it per level made a token of alternating
/// emphasis characters quadratic (256 KB of `-_-_...X..._-_-` took 14 seconds
/// on the write path).
fn separates_a_word(tok: &str) -> bool {
    let first = match tok.find(|c: char| c.is_alphanumeric()) {
        Some(i) => i,
        None => return false,
    };
    let last = match tok.rfind(|c: char| c.is_alphanumeric()) {
        Some(i) => i,
        None => return false,
    };
    tok[first..last].bytes().any(|b| b == b'-' || b == b'_')
}

/// Peel every level of BALANCED markdown emphasis (`_KEY_`, `__KEY__`,
/// `--KEY--`, and mixed nests like `-_KEY_-`) off a candidate token.
///
/// `-` and `_` cannot be trimmed the way a period is. They are credential body
/// characters, and the Google gate is an exact `n == 39` over a charset that
/// CONTAINS both, so a maximal `trim_matches` peel eats the credential's own
/// final byte: `_AIza...qrstu-_` is 41 bytes and peeling every edge `-`/`_`
/// leaves 38, the wrapper and the key's last character gone in one pass. That
/// is exactly the failure `trim_edge_punctuation` above warns about.
///
/// Markdown emphasis is BALANCED, so the wrapper can be told apart from a body
/// character by SYMMETRY: peel a pair only when the SAME character sits on both
/// edges. A key's own trailing `-` has no matching leading `-`, so it survives,
/// while the `_` pair around it comes off.
///
/// Symmetry alone is not enough. `_sk_test_refund_path_covered_here_` and
/// `__rk_live_reload_watcher_thread__` are balanced too, and peeling them
/// MANUFACTURES a provider prefix the source text never contained, so `remember`
/// would start refusing ordinary snake_case identifiers and CSS custom
/// properties. So the interior has to be opaque as well, which is what `opaque`
/// (`separates_a_word` over the whole candidate) carries: when a body character
/// separates words inside it, the edge pair is part of the token's own spelling,
/// not emphasis around a credential.
///
/// Every level comes off in ONE call, and nothing is skipped by that: each
/// intermediate form still STARTS with a body character, and every rule in
/// `classify_token` is anchored on an alphanumeric prefix, so no intermediate
/// form can classify. Peeling one level per contraction only re-walked the same
/// token once per level.
///
/// The cost is a credential that is BOTH emphasised and spelled with body
/// characters (`_ghp_...token_`), which stays unclassified. That is the shape
/// the pre-0.16.1 detector missed as well, and it is the safe side of the trade:
/// the alternative refuses every snake_case identifier a user tries to store.
fn peel_balanced_emphasis(tok: &str, opaque: bool) -> &str {
    if !opaque {
        return tok;
    }
    let b = tok.as_bytes();
    let mut lo = 0;
    let mut hi = b.len();
    // `>= 3` keeps at least one byte of interior: a token that is nothing but
    // the run has nothing to classify.
    while hi - lo >= 3 {
        let c = b[lo];
        if (c != b'-' && c != b'_') || b[hi - 1] != c {
            break;
        }
        lo += 1;
        hi -= 1;
    }
    &tok[lo..hi]
}

/// Peel ONE trailing `-`/`_`.
///
/// Unlike a leading peel this needs no symmetry, because it cannot manufacture
/// anything: every rule in `classify_token` is anchored at byte 0, so trimming
/// the tail can only expose a SHORTER token, and the classifier has already
/// seen the longer one. One character at a time, not the whole run, because the
/// exact-length gates have to be offered every intermediate width.
fn peel_trailing_body(tok: &str) -> &str {
    match tok.as_bytes().last() {
        Some(&c) if c == b'-' || c == b'_' => &tok[..tok.len() - 1],
        _ => tok,
    }
}

/// The outcome of one contraction step.
///
/// Which EDGE moved matters, not just how many bytes came off: the JWT rules
/// below are anchored on the front of the token, so they only have to be
/// re-offered when the front has actually shifted.
enum Step<'a> {
    /// The leading edge moved (or a maximal trim moved both), so the dot
    /// segments have shifted underneath the JWT rules.
    Front(&'a str),
    /// Only the trailing edge moved.
    Tail(&'a str),
    /// A fixed point: nothing more comes off.
    Done,
}

/// One contraction step, cheapest rule that still has work to do.
fn contract(tok: &str, opaque: bool) -> Step<'_> {
    let trimmed = trim_edge_punctuation(tok);
    if trimmed.len() != tok.len() {
        return Step::Front(trimmed);
    }
    let peeled = peel_balanced_emphasis(tok, opaque);
    if peeled.len() != tok.len() {
        return Step::Front(peeled);
    }
    // A token that still STARTS with a body character is finished. Every rule
    // in `classify_token` is anchored on an alphanumeric prefix, so no amount
    // of TRAILING peeling can ever make this token classify, and the leading
    // run has already been refused as unbalanced or as an identifier's own
    // separator. Stopping here is not just an optimisation: without it a
    // markdown horizontal rule (`------...`) is contracted one byte per
    // iteration while every iteration rescans the run, which is quadratic in
    // the length of a token an attacker controls.
    match tok.as_bytes().first() {
        Some(&c) if c == b'-' || c == b'_' => Step::Done,
        _ => {
            let tail = peel_trailing_body(tok);
            if tail.len() == tok.len() {
                Step::Done
            } else {
                Step::Tail(tail)
            }
        }
    }
}

/// The first three dot segments of `s`, or all of `s` when it holds fewer than
/// three.
fn three_segments(s: &str) -> &str {
    let mut dots = 0;
    for (i, b) in s.bytes().enumerate() {
        if b == b'.' {
            dots += 1;
            if dots == 3 {
                return &s[..i];
            }
        }
    }
    s
}

/// A JWT glued to the next sentence (`<jwt>.Then redeploy`) or trailed by a
/// stray body character (`<jwt>._`) splits into MORE than three dot segments,
/// and `classify_token`'s JWT rule is whole-token-exact (`parts.len() == 3`).
/// `detect`'s period re-split cannot rescue it either: re-splitting shreds the
/// JWT into its own three segments and none of them is a JWT on its own. That
/// is why the same glued shape is caught for AWS and missed for JWT.
///
/// The `eyJ` anchor is at the very start here, so the only candidate worth
/// re-offering is the FIRST three segments. Every gate stays where it was:
/// `classify_token` sees the candidate exactly as it sees a bare JWT, and
/// applies the same non-empty, base64url and `n >= 40` tests (I-A2).
fn jwt_prefix_candidate(tok: &str) -> Option<&str> {
    if !tok.starts_with("eyJ") {
        return None;
    }
    let head = three_segments(tok);
    // Three segments or fewer: `classify_token` has already seen this whole.
    if head.len() == tok.len() {
        return None;
    }
    Some(head)
}

/// The same collision on the OTHER side. `rotate now.<jwt>` and `prod.<jwt>`
/// glue a JWT to the text BEFORE it, and there the first three segments are
/// `now`, the header and the payload, so `jwt_prefix_candidate` cannot see it
/// either. AWS never had the problem in either direction because `detect`'s
/// period re-split finds a bare `AKIA...` segment; a JWT is the one credential
/// that re-split destroys, which is the whole of the asymmetry the audit named.
///
/// The anchor is a `.eyJ`, so every candidate window is a contiguous slice that
/// starts at a dot SEGMENT: nothing is manufactured, and `classify_token`
/// applies its unchanged gates to it (I-A2).
///
/// Interior anchors are INVARIANT under contraction (each contraction strips
/// edges only, so a `.` that precedes an anchor stays interior), so this runs
/// ONCE over the raw candidate instead of inside the peel loop.
fn classify_interior_jwt_windows(raw: &str) -> Option<&'static str> {
    let mut from = 0;
    while let Some(rel) = raw[from..].find(".eyJ") {
        let at = from + rel + 1;
        from = at + 3;
        if let Some(label) = classify_token(three_segments(&raw[at..])) {
            return Some(label);
        }
    }
    None
}

/// The most contractions `classify_candidate` will spend on one token. See
/// the cost paragraph on `classify_candidate` for why this exists and why 64
/// is far beyond any accidental decoration depth.
const CONTRACTION_BUDGET: usize = 64;

/// Classify one candidate token, contracting it to a FIXED POINT.
///
/// Two boundary rules apply, and they pull against each other:
/// `trim_edge_punctuation` peels punctuation but keeps `-`/`_`, and
/// `peel_balanced_emphasis` peels a matched `-`/`_` pair. Running each once, in
/// a fixed order, leaves characters only the OTHER pass can see: `_<jwt>._`
/// stops the trim at the exempt trailing `_`, and peeling that `_` then EXPOSES
/// a trailing `.` that nothing removes, so the JWT's three segments become four
/// and the gate misses. They therefore alternate in a loop, and the token is
/// classified after EVERY single contraction, so a character exposed by one
/// pass is always seen by the other.
///
/// Every iteration either stops or drops at least one byte, but that alone
/// does not keep the cost linear: classifying after every contraction rescans
/// the token, so a shape that forces one contraction per byte (a valid prefix,
/// a long alphanumeric run, then a long trailing `-`/`_` run peeled one byte
/// at a time) is O(n^2) in a length the writer controls, and at the body cap
/// that is hundreds of milliseconds per call on paths (evidence commands,
/// import lines) that arrive in bulk. `CONTRACTION_BUDGET` bounds the loop
/// instead: the trim and the emphasis peel each take a maximal run or every
/// balanced level in ONE step, so real decoration (quotes, emphasis, sentence
/// punctuation, even nested combinations) is gone within a handful of
/// iterations, and only adversarial alternations or decoration runs far past
/// any accidental paste ever reach the budget. A token still undecided after
/// 64 contractions is decoration, not a credential a human pasted; stopping
/// there keeps the cost at O(budget * n) = linear in the token.
///
/// A tail peel cannot change `jwt_prefix_candidate`'s answer: it fires only on
/// four or more segments, and it reads the first three, none of which is the
/// segment a tail peel touches.
///
/// No pass touches a length or charset rule. They are all boundary rules, and
/// `AKIA` + 16 alnum + `X` is still not a key after any of them (I-A2).
fn classify_candidate(raw: &str) -> Option<&'static str> {
    let opaque = !separates_a_word(raw);
    let mut tok = raw;
    let mut front_moved = true;
    for _ in 0..CONTRACTION_BUDGET {
        if let Some(label) = classify_token(tok) {
            return Some(label);
        }
        if front_moved {
            if let Some(head) = jwt_prefix_candidate(tok) {
                if let Some(label) = classify_token(head) {
                    return Some(label);
                }
            }
        }
        match contract(tok, opaque) {
            Step::Front(next) => {
                tok = next;
                front_moved = true;
            }
            Step::Tail(next) => {
                tok = next;
                front_moved = false;
            }
            Step::Done => break,
        }
    }
    classify_interior_jwt_windows(raw)
}

fn classify_token(tok: &str) -> Option<&'static str> {
    let n = tok.len();

    // AWS access key id: AKIA/ASIA + 16 uppercase alnum.
    if (tok.starts_with("AKIA") || tok.starts_with("ASIA"))
        && n == 20
        && tok[4..].bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    {
        return Some("AWS access key id");
    }

    // GitHub tokens: ghp_/gho_/ghu_/ghs_/ghr_ + >=36, or github_pat_.
    if let Some(rest) = tok
        .strip_prefix("ghp_")
        .or_else(|| tok.strip_prefix("gho_"))
        .or_else(|| tok.strip_prefix("ghu_"))
        .or_else(|| tok.strip_prefix("ghs_"))
        .or_else(|| tok.strip_prefix("ghr_"))
    {
        if rest.len() >= 36 && rest.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Some("GitHub token");
        }
    }
    if tok.starts_with("github_pat_") && n >= 40 {
        return Some("GitHub token");
    }

    // Slack tokens: xoxb-/xoxp-/xoxa-/xoxr-/xoxs- + a credential-shaped
    // body. Three gates keep prose like "xoxo-hugs-and-kisses" out
    // (audit 2026-07): a real variant letter, token charset, and at least
    // one digit (Slack bodies always carry numeric segments).
    if tok.starts_with("xox")
        && n >= 20
        && matches!(tok.as_bytes().get(3), Some(b'b' | b'p' | b'a' | b'r' | b's'))
        && tok.as_bytes().get(4) == Some(&b'-')
        && tok[5..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && tok[5..].bytes().any(|b| b.is_ascii_digit())
    {
        return Some("Slack token");
    }

    // OpenAI-style keys: sk- (and sk-proj-) + long base62-ish body.
    if let Some(rest) = tok.strip_prefix("sk-") {
        let body = rest.strip_prefix("proj-").unwrap_or(rest);
        if body.len() >= 20 && body.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            return Some("API secret key");
        }
    }

    // Stripe live/test secret keys.
    if (tok.starts_with("sk_live_") || tok.starts_with("sk_test_") || tok.starts_with("rk_live_"))
        && n >= 24
    {
        return Some("Stripe secret key");
    }

    // Google API key: AIza + 35 url-safe chars.
    if tok.starts_with("AIza")
        && n == 39
        && tok[4..].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Some("Google API key");
    }

    // JSON Web Token: three base64url segments separated by dots. Require real
    // length so short "a.b.c" style prose does not trip it.
    if tok.starts_with("eyJ") {
        let parts: Vec<&str> = tok.split('.').collect();
        if parts.len() == 3
            && n >= 40
            && parts.iter().all(|p| {
                !p.is_empty()
                    && p.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            })
        {
            return Some("JWT");
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_real_credentials() {
        // Fixtures are split with concat! so external secret scanners
        // (GitHub, trufflehog) do not flag the detector's own tests; the
        // assembled runtime value still matches each provider pattern.
        assert_eq!(
            detect(concat!("key is ", "AKIAIOSFOD", "NN7EXAMPLE", " here")),
            Some("AWS access key id")
        );
        assert_eq!(
            detect(concat!("token ghp_", "1234567890abcdefghijklmnopqrstuvwxyz")),
            Some("GitHub token")
        );
        assert_eq!(
            detect(concat!("xoxb-", "123456789012-abcdefghijkl")),
            Some("Slack token")
        );
        assert_eq!(
            detect(concat!("sk-proj-", "abcdefghijklmnopqrstuvwxyz0123456789")),
            Some("API secret key")
        );
        assert_eq!(
            detect(concat!("AIzaSyD", "1234567890abcdefghijklmnopqrstuv")),
            Some("Google API key")
        );
        assert_eq!(
            detect("-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END OPENSSH PRIVATE KEY-----"),
            Some("private key block")
        );
        assert!(detect("Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcDEFghiJKL").is_some());
    }

    #[test]
    fn env_and_yaml_forms_are_caught() {
        // The standard .env / YAML shapes must split on = and : so the
        // credential body is inspected (audit 2026-07).
        assert_eq!(
            detect(concat!("AWS_KEY=", "AKIAIOSFOD", "NN7EXAMPLE")),
            Some("AWS access key id")
        );
        assert_eq!(
            detect(concat!("token: ghp_", "1234567890abcdefghijklmnopqrstuvwxyz")),
            Some("GitHub token")
        );
        assert_eq!(
            detect(concat!("{\"key\":\"sk-proj-", "abcdefghijklmnopqrstuvwxyz0123456789", "\"}")),
            Some("API secret key")
        );
    }

    #[test]
    fn xoxo_prose_is_not_a_slack_token() {
        assert_eq!(detect("sending xoxo-hugs-and-kisses-to-everyone!!!"), None);
    }

    #[test]
    fn ignores_ordinary_prose_and_code_names() {
        assert_eq!(detect("the endpoint returns a bearer token on login"), None);
        assert_eq!(detect("recall applies a 0.35 relative score cutoff"), None);
        assert_eq!(detect("call sk_from_context() then skip the akia branch"), None);
        assert_eq!(detect("main.rs uses a hand-rolled arg parser, not clap"), None);
        assert_eq!(detect("a.b.c is a dotted path, not a JWT"), None);
    }

    // --- punctuation boundary suite (audit 2026-08, I-A1 / I-A2) ---
    //
    // A credential is almost never written bare. It lands at the end of a
    // sentence, inside markdown emphasis, or in parentheses. Before 0.16.1 the
    // split set omitted sentence and markdown punctuation, so
    // `AKIA...EXAMPLE.` reached the classifier as a 21-byte token and missed
    // the `n == 20` gate; the secret was then stored and exported into a
    // git-tracked JSONL. Every provider rule is re-tested in all three shapes.

    // The 0.16.1 review's structural finding: the old vocabulary was three
    // shapes (period, bold, parens), and every OTHER test varied one dimension
    // while holding the rest safe. Every leak it missed lived in an uncovered
    // INTERSECTION: a Google key that both ends in a body character AND sits in
    // underscore emphasis; a JWT that is both emphasised AND period-terminated;
    // a JWT glued to the next sentence, a shape that was only ever tested on
    // the two providers the mechanism already handled. So the vocabulary is now
    // a MATRIX: every provider fixture (each one twice, once ending in an
    // alphanumeric and once ending in a body character) against every wrapper.

    /// Wrappers a credential must still be found inside. None of them changes
    /// the fixture's own bytes, so they are safe to run over the negative
    /// fixtures too.
    const WRAPPERS: &[(&str, &str)] = &[
        ("bare", "{K}"),
        ("trailing period", "the key is {K}."),
        ("ascii ellipsis", "the key is {K}..."),
        ("unicode ellipsis", "the key is {K}\u{2026}"),
        ("smart quotes", "the key is \u{201c}{K}\u{201d}."),
        ("guillemets", "\u{ab}{K}\u{bb}"),
        ("fullwidth colon", "key\u{ff1a}{K}"),
        ("em dash", "the key is {K}\u{2014}rotate"),
        ("bold", "the key is **{K}** ok"),
        ("parens", "the key is ({K}) ok"),
        ("parens then period", "the key is ({K}.)"),
        ("code fence", "```\n{K}\n```"),
        ("markdown list", "- key: {K}"),
        ("url query string", "https://api.example.com/v1?key={K}&callback=init"),
        ("yaml", "token: {K}"),
        ("dotenv", "AWS_SECRET={K}"),
        ("period then underscore", "the key is {K}._"),
        ("period then dash", "the key is {K}.-"),
        ("glued next sentence", "rotate {K}.Then redeploy"),
        ("glued previous sentence", "rotate now.{K}"),
        ("dotted suffix", "{K}.prod"),
        ("dotted prefix", "prod.{K}"),
    ];

    /// Wrappers that APPEND a body character to the fixture. Real keys must
    /// still be found inside them, but they cannot be run over the
    /// length-edge negatives: appending `_` to a 38-byte `AIza` fixture
    /// produces a genuine 39-byte key, so the "negative" stops being one.
    const BODY_CHAR_WRAPPERS: &[(&str, &str)] = &[
        ("trailing dash", "the key is {K}-"),
        ("trailing underscore", "the key is {K}_"),
    ];

    /// Wrappers whose leading `-`/`_` run has no match on the other edge.
    /// These are deliberately NOT peeled: an unbalanced leading peel is what
    /// manufactured `sk-`/`sk_test_`/`rk_live_` prefixes out of CSS custom
    /// properties and snake_case identifiers.
    ///
    /// The third field is the leading run itself. A fixture that ENDS in that
    /// exact run makes the pair BALANCED, which is a different shape (and one
    /// the peel is allowed to open), so those combinations are skipped rather
    /// than asserted the wrong way round.
    const ASYMMETRIC_WRAPPERS: &[(&str, &str, &str)] = &[
        ("leading dash", "the key is -{K}", "-"),
        ("leading underscore", "the key is _{K}", "_"),
        ("leading double dash", "the key is --{K}", "--"),
        ("leading double underscore", "the key is __{K}", "__"),
    ];

    /// Balanced markdown emphasis wrapped TIGHTLY around the credential, so the
    /// emphasis characters land inside the candidate token and the peel is what
    /// decides the verdict.
    const EMPHASIS_WRAPPERS: &[(&str, &str)] = &[
        ("underscore emphasis", "the key is _{K}_ ok"),
        ("underscore bold", "the key is __{K}__ ok"),
        ("dash emphasis", "the key is -{K}- ok"),
        ("dash bold", "the key is --{K}-- ok"),
        ("emphasis inside a list item", "- _{K}._"),
    ];

    /// Emphasis around the whole SENTENCE. The opening delimiter sits on
    /// another word, so the credential's own token carries nothing but a
    /// trailing `._` and the peel never sees a balanced pair at all. Every
    /// provider must still be found, whatever the fixture is spelled with.
    /// This is the shape BLOCKER 2 hid in.
    const DETACHED_EMPHASIS_WRAPPERS: &[(&str, &str)] = &[
        ("emphasised sentence", "_the token is {K}._"),
        ("bold sentence", "__the token is {K}.__"),
        ("dash emphasised sentence", "-the token is {K}.-"),
    ];

    /// Every provider, twice: once ending in an alphanumeric and once ending
    /// in a body character, wherever the provider's own charset allows one.
    /// The body-char fixtures are the whole point of the exercise: they are
    /// the ones an over-eager peel silently shortens past its gate.
    fn fixtures() -> Vec<(&'static str, String, &'static str)> {
        vec![
            ("aws akia", concat!("AKIAIOSFOD", "NN7EXAMPLE").into(), "AWS access key id"),
            ("aws asia", concat!("ASIAIOSFOD", "NN7EXAMPLE").into(), "AWS access key id"),
            ("github ghp", concat!("ghp_", "1234567890abcdefghijklmnopqrstuvwxyz").into(), "GitHub token"),
            ("github pat", concat!("github_pat_", "11ABCDE0000abcdefghijklmnopqrstuvwxyz0123456789").into(), "GitHub token"),
            ("github pat ending _", concat!("github_pat_", "11ABCDE0000abcdefghijklmnopqrstuvwxyz012345678_").into(), "GitHub token"),
            ("slack xoxb", concat!("xoxb-", "123456789012-abcdefghijkl").into(), "Slack token"),
            ("slack xoxp", concat!("xoxp-", "123456789012-abcdefghijkl").into(), "Slack token"),
            ("slack ending -", concat!("xoxb-", "123456789012-abcdefghijk-").into(), "Slack token"),
            ("openai sk", concat!("sk-", "abcdefghijklmnopqrstuvwxyz0123456789").into(), "API secret key"),
            ("openai sk-proj", concat!("sk-proj-", "abcdefghijklmnopqrstuvwxyz0123456789").into(), "API secret key"),
            ("openai ending _", concat!("sk-proj-", "abcdefghijklmnopqrstuvwxyz012345678_").into(), "API secret key"),
            ("stripe sk_live", concat!("sk_live_", "51abcdefghijklmnopqrstuvwx").into(), "Stripe secret key"),
            ("stripe sk_test", concat!("sk_test_", "51abcdefghijklmnopqrstuvwx").into(), "Stripe secret key"),
            ("stripe rk_live", concat!("rk_live_", "51abcdefghijklmnopqrstuvwx").into(), "Stripe secret key"),
            ("stripe ending _", concat!("sk_live_", "51abcdefghijklmnopqrstuv_").into(), "Stripe secret key"),
            ("google", concat!("AIzaSyD", "1234567890abcdefghijklmnopqrstuv").into(), "Google API key"),
            ("google ending -", concat!("AIzaSyD", "1234567890abcdefghijklmnopqrstu-").into(), "Google API key"),
            ("google ending _", concat!("AIzaSyD", "1234567890abcdefghijklmnopqrstu_").into(), "Google API key"),
            ("jwt", "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcDEFghiJKL".into(), "JWT"),
            ("jwt ending -", "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcDEFghiJK-".into(), "JWT"),
            ("jwt ending _", "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcDEFghiJK_".into(), "JWT"),
        ]
    }

    /// Mirrors `separates_a_word`: true when the fixture carries no `-`/`_`
    /// between its first and its last alphanumeric, so balanced emphasis around
    /// it can be told apart from the fixture's own spelling. A key's own
    /// TRAILING body character sits outside that span and does not count, which
    /// is exactly the distinction that keeps a 39-byte `AIza...qrstu_`
    /// reachable inside `_..._`.
    fn is_opaque(key: &str) -> bool {
        match (
            key.find(|c: char| c.is_alphanumeric()),
            key.rfind(|c: char| c.is_alphanumeric()),
        ) {
            (Some(f), Some(l)) => !key[f..l].contains('-') && !key[f..l].contains('_'),
            _ => true,
        }
    }

    fn assert_every_shape(key: &str, want: &str) {
        for (name, tmpl) in WRAPPERS.iter().chain(BODY_CHAR_WRAPPERS) {
            let s = tmpl.replace("{K}", key);
            assert_eq!(detect(&s), Some(want), "missed {want} in {name}: {s}");
        }
    }

    /// Negatives skip `BODY_CHAR_WRAPPERS`: those append a byte to the fixture,
    /// and a length-edge negative plus one body character is a real key.
    fn assert_no_shape_matches(key: &str) {
        for (name, tmpl) in WRAPPERS {
            let s = tmpl.replace("{K}", key);
            assert_eq!(detect(&s), None, "false positive in {name}: {s}");
        }
    }

    /// The matrix. Every provider, every wrapper, both fixture endings.
    #[test]
    fn every_provider_is_caught_in_every_wrapper() {
        for (label, key, want) in fixtures() {
            for (name, tmpl) in WRAPPERS.iter().chain(BODY_CHAR_WRAPPERS) {
                let s = tmpl.replace("{K}", &key);
                assert_eq!(detect(&s), Some(want), "missed {label} in {name}: {s}");
            }
        }
    }

    /// Balanced emphasis is peeled, and the credential's own trailing body
    /// character survives the peel (that is the `n == 39` Google gate).
    ///
    /// The exception is a fixture that spells itself with `-`/`_`: there the
    /// edge pair is indistinguishable from the token's own separators, so the
    /// peel is refused. That refusal is not incidental, it is the fix for the
    /// false positives below, and it costs only shapes the pre-0.16.1 detector
    /// missed as well.
    #[test]
    fn balanced_emphasis_is_peeled_unless_the_fixture_spells_itself_with_body_chars() {
        for (label, key, want) in fixtures() {
            let expect = if is_opaque(&key) { Some(want) } else { None };
            for (name, tmpl) in EMPHASIS_WRAPPERS {
                let s = tmpl.replace("{K}", &key);
                assert_eq!(detect(&s), expect, "{label} in {name}: {s}");
            }
        }
    }

    /// Emphasis around the whole sentence never reaches the peel, so it costs
    /// nothing: every provider is still caught, including the ones spelled with
    /// body characters that tight emphasis has to give up on.
    #[test]
    fn sentence_level_emphasis_never_hides_a_credential() {
        for (label, key, want) in fixtures() {
            for (name, tmpl) in DETACHED_EMPHASIS_WRAPPERS {
                let s = tmpl.replace("{K}", &key);
                assert_eq!(detect(&s), Some(want), "{label} in {name}: {s}");
            }
        }
    }

    /// An UNBALANCED leading run is never peeled, for any provider. This is the
    /// rule that stops a peel from manufacturing a provider prefix the source
    /// text never contained.
    #[test]
    fn asymmetric_leading_runs_are_never_peeled() {
        for (label, key, _) in fixtures() {
            for (name, tmpl, run) in ASYMMETRIC_WRAPPERS {
                // The fixture's own trailing body character can close the
                // wrapper's leading run. That pair is balanced, not asymmetric.
                if key.ends_with(run) {
                    continue;
                }
                let s = tmpl.replace("{K}", &key);
                assert_eq!(detect(&s), None, "{label} peeled in {name}: {s}");
            }
        }
    }

    /// The write-path regression the peel introduced: ordinary CSS custom
    /// properties and snake_case identifiers were refused by `remember`,
    /// because an unbalanced peel manufactured `sk-`, `sk_test_` and `rk_live_`
    /// prefixes out of them. The emphasis character and the identifier's own
    /// separator do not have to match, so both spellings are pinned.
    #[test]
    fn identifiers_and_css_custom_properties_are_not_credentials() {
        for s in [
            "the css token is --sk-color-surface-elevated-default on dark",
            "set --sk-typography-heading-scale-ratio in the theme file",
            "the fixture is named _sk_test_refund_path_covered_here_",
            "helper __rk_live_reload_watcher_thread__ owns the fs watch",
            // the same four wearing the other delimiter
            "--rk_live_reload_watcher_thread-- owns the fs watch",
            "the helper --sk_test_refund_path_covered_here-- is a fixture",
            "__sk-color-surface-elevated-default__ on dark",
            "the css token is __sk-typography-heading-scale-ratio__ here",
            // and unwrapped, the shape a stylesheet actually carries
            "--sk-color-surface-elevated-default: #fff;",
            "a helper called _sk_test_helper_ lives in tests",
        ] {
            assert_eq!(detect(s), None, "false positive on: {s}");
        }
    }

    /// A markdown horizontal rule is a single token of nothing but body
    /// characters. It can never classify, and contracting it one byte at a
    /// time while rescanning the run is quadratic in a length the writer
    /// controls, so the loop has to bail out of it immediately.
    #[test]
    fn a_long_body_char_run_is_not_quadratic() {
        let rule = "-".repeat(200_000);
        let start = std::time::Instant::now();
        assert_eq!(detect(&rule), None);
        assert_eq!(detect(&"_".repeat(200_000)), None);
        assert_eq!(detect(&format!("see below\n{rule}\nand above")), None);
        assert!(
            start.elapsed().as_secs() < 5,
            "the peel loop went quadratic: {:?}",
            start.elapsed()
        );
    }

    /// A UNIFORM run is the easy case, and testing only that hid the real one.
    /// When the emphasis characters ALTERNATE, every level is a separate
    /// balanced pair, so a peel that takes one level per contraction and
    /// re-derives the interior's opacity each time walks the token once per
    /// level. That is 256 KB of ordinary `remember` body text costing 14
    /// seconds. The opacity test is an invariant of the candidate and the peel
    /// takes every level in one step, so this has to stay linear.
    #[test]
    fn alternating_emphasis_characters_are_not_quadratic() {
        let start = std::time::Instant::now();
        for k in [16_000, 64_000] {
            let s = format!("{}X{}", "-_".repeat(k), "_-".repeat(k));
            assert_eq!(detect(&s), None, "k={k}");
        }
        assert_eq!(detect(&format!("{}X{}", "._".repeat(64_000), "_.".repeat(64_000))), None);
        assert!(
            start.elapsed().as_secs() < 5,
            "the peel loop went quadratic: {:?}",
            start.elapsed()
        );
    }

    /// The 2026-08-25 review round found the shape BOTH tests above miss: a
    /// valid provider prefix, a long alphanumeric run, then a long trailing
    /// `-`/`_` run. The token starts alphanumeric, so the horizontal-rule
    /// bail-out never fires, and the tail is peeled one byte per contraction
    /// while every contraction rescans the alphanumeric run: 64 KB of it
    /// measured ~640 ms per `detect`, on paths (evidence commands, import
    /// lines) with no length cap. `CONTRACTION_BUDGET` bounds the loop; this
    /// pins the bound for each provider family the review timed.
    #[test]
    fn a_prefixed_token_with_a_long_trailing_run_is_not_quadratic() {
        let start = std::time::Instant::now();
        let alnum = "a".repeat(128_000);
        let run = "-".repeat(128_000);
        assert_eq!(detect(&format!("ghp_{alnum}{run}")), None);
        // The dotted sk- shape still classifies, through the period re-split's
        // clean segment, not through the contraction walk the budget cut off.
        assert_eq!(detect(&format!("sk-{alnum}.{run}")), Some("API secret key"));
        assert_eq!(detect(&format!("eyJ{alnum}{}", "_".repeat(128_000))), None);
        assert!(
            start.elapsed().as_secs() < 5,
            "the contraction loop went quadratic: {:?}",
            start.elapsed()
        );
    }

    /// The budget is a DoS bound, not a detection change: every real
    /// credential wearing accidental decoration is classified in a handful of
    /// contractions, far under `CONTRACTION_BUDGET`. A key carrying a short
    /// trailing stray run must still be caught after the cap lands.
    #[test]
    fn the_contraction_budget_leaves_decorated_credentials_detected() {
        let key = concat!("ghp_", "1234567890abcdefghijklmnopqrstuvwxyz");
        assert_eq!(detect(&format!("{key}{}", "_".repeat(10))), Some("GitHub token"));
        assert_eq!(
            detect(&format!("token ({key}__), rotate it")),
            Some("GitHub token")
        );
    }

    /// BLOCKER 3 named the RIGHT-hand collision (`<jwt>.Then redeploy`).
    /// Gluing text to the LEFT is the same shape and the same asymmetry: AWS is
    /// caught either way by `detect`'s period re-split, and a JWT is the one
    /// credential that re-split shreds, so it needs a window anchored on the
    /// interior `.eyJ` as well as one anchored on the front.
    #[test]
    fn jwts_glued_to_preceding_text_are_caught() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcDEFghiJKL";
        for s in [
            format!("rotate now.{jwt}"),
            format!("prod.{jwt}"),
            format!("see config.{jwt}.Then redeploy"),
            format!("the token is _config.{jwt}._"),
            format!("a.b.c.{jwt}"),
        ] {
            assert_eq!(detect(&s), Some("JWT"), "missed the glued JWT in: {s}");
        }
        // The control the mechanism already handled, in the same shape.
        assert_eq!(
            detect(concat!("rotate now.", "AKIAIOSFOD", "NN7EXAMPLE")),
            Some("AWS access key id")
        );
    }

    /// I-A2 over the window scan. A window is a contiguous slice anchored at a
    /// dot segment that starts with `eyJ`, and `classify_token` applies exactly
    /// the gates it applies to a bare token: three non-empty base64url segments
    /// and at least 40 bytes. Nothing is manufactured and nothing is loosened.
    #[test]
    fn the_jwt_window_scan_does_not_manufacture_a_token() {
        for s in [
            "eyJ starts a JWT.",
            "a.b.c is a dotted path, not a JWT.",
            // `eyJ` mid-word is not a segment start.
            "the keyeyJab.cd.ef.gh is not a token",
            // segments are there, the 40 byte floor is not.
            "eyJa.b.c.d",
            "release.notes.eyJa.short.tail",
            // an empty segment still fails, however long the rest is.
            "prod.eyJhbGciOiJIUzI1NiJ9..eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcDEFghiJKL",
        ] {
            assert_eq!(detect(s), None, "false positive on: {s}");
        }
    }

    #[test]
    fn aws_keys_are_caught_in_every_prose_shape() {
        assert_every_shape(concat!("AKIAIOSFOD", "NN7EXAMPLE"), "AWS access key id");
        assert_every_shape(concat!("ASIAIOSFOD", "NN7EXAMPLE"), "AWS access key id");
    }

    #[test]
    fn github_tokens_are_caught_in_every_prose_shape() {
        assert_every_shape(
            concat!("ghp_", "1234567890abcdefghijklmnopqrstuvwxyz"),
            "GitHub token",
        );
        assert_every_shape(
            concat!("github_pat_", "11ABCDE0000abcdefghijklmnopqrstuvwxyz0123456789"),
            "GitHub token",
        );
    }

    #[test]
    fn slack_tokens_are_caught_in_every_prose_shape() {
        assert_every_shape(concat!("xoxb-", "123456789012-abcdefghijkl"), "Slack token");
        assert_every_shape(concat!("xoxp-", "123456789012-abcdefghijkl"), "Slack token");
    }

    #[test]
    fn openai_keys_are_caught_in_every_prose_shape() {
        assert_every_shape(
            concat!("sk-", "abcdefghijklmnopqrstuvwxyz0123456789"),
            "API secret key",
        );
        assert_every_shape(
            concat!("sk-proj-", "abcdefghijklmnopqrstuvwxyz0123456789"),
            "API secret key",
        );
    }

    #[test]
    fn stripe_keys_are_caught_in_every_prose_shape() {
        assert_every_shape(
            concat!("sk_live_", "51abcdefghijklmnopqrstuvwx"),
            "Stripe secret key",
        );
        assert_every_shape(
            concat!("sk_test_", "51abcdefghijklmnopqrstuvwx"),
            "Stripe secret key",
        );
        assert_every_shape(
            concat!("rk_live_", "51abcdefghijklmnopqrstuvwx"),
            "Stripe secret key",
        );
    }

    #[test]
    fn google_keys_are_caught_in_every_prose_shape() {
        assert_every_shape(
            concat!("AIzaSyD", "1234567890abcdefghijklmnopqrstuv"),
            "Google API key",
        );
    }

    #[test]
    fn jwts_are_caught_in_every_prose_shape() {
        assert_every_shape(
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcDEFghiJKL",
            "JWT",
        );
    }

    #[test]
    fn private_key_blocks_are_caught_in_every_prose_shape() {
        assert_every_shape(
            "-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END OPENSSH PRIVATE KEY-----",
            "private key block",
        );
    }

    #[test]
    fn jwt_segments_survive_the_period_boundary() {
        // A period ends a sentence but also joins a JWT's three segments, so
        // the whole token has to be classified BEFORE any period re-split.
        assert_eq!(
            detect("token=eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcDEFghiJKL."),
            Some("JWT")
        );
    }

    #[test]
    fn keys_glued_to_neighbouring_text_are_caught() {
        // No space after the period, and a key wrapped in ellipses.
        assert_eq!(
            detect(concat!("rotate ", "AKIAIOSFOD", "NN7EXAMPLE", ".Then redeploy")),
            Some("AWS access key id")
        );
        assert_eq!(
            detect(concat!("...", "AKIAIOSFOD", "NN7EXAMPLE", "...")),
            Some("AWS access key id")
        );
        assert_eq!(
            detect(concat!("#deploy: ", "ghp_", "1234567890abcdefghijklmnopqrstuvwxyz", "!")),
            Some("GitHub token")
        );
    }

    /// I-A2: the trim is a boundary rule, never a charset relaxation. A token
    /// the classifier rejected bare must still be rejected wrapped.
    #[test]
    fn trimming_never_loosens_a_length_or_charset_gate() {
        // AKIA + 16 alnum + one extra byte: 21 long, still not a key.
        assert_no_shape_matches(concat!("AKIAIOSFOD", "NN7EXAMPLEX"));
        // Lowercase in the AWS body is out of charset at any length.
        assert_no_shape_matches(concat!("AKIAIOSFOD", "NN7example"));
        // One char short of the Google key length, and one char over.
        assert_no_shape_matches(concat!("AIzaSyD", "1234567890abcdefghijklmnopqrstu"));
        assert_no_shape_matches(concat!("AIzaSyD", "1234567890abcdefghijklmnopqrstuvw"));
        // A GitHub token body one char short of 36.
        assert_no_shape_matches(concat!("ghp_", "1234567890abcdefghijklmnopqrstuvwxy"));
    }

    /// The widened boundary lets more candidates reach the classifier, so the
    /// false-positive floor is re-tested at sentence and markdown edges.
    #[test]
    fn prose_at_punctuation_boundaries_still_stores_cleanly() {
        for s in [
            "rotate the sk-key.",
            "sending xoxo-hugs-and-kisses.",
            "the endpoint returns a bearer token.",
            "we shipped the INTERNATIONALIZATION.",
            "see **the sk-key** in the runbook.",
            "the prefix is AKIA and the rest is random.",
            "AIza is the Google prefix, ghp_ is GitHub's.",
            "eyJ starts a JWT.",
            "*emphasis* around sk-proj. nothing more.",
            "main.rs holds the arg parser.",
            "a.b.c is a dotted path, not a JWT.",
            "#deploy notes: rotate every credential quarterly!",
            "ASIA and AKIA are both AWS prefixes, so is ASIA.",
            "the store is at ~/.limpet/memory.jsonl.",
        ] {
            assert_eq!(detect(s), None, "false positive on: {s}");
        }
    }

    // --- class boundary suite (review round 2, I-A1 / I-A2) ---
    //
    // The first cut of the punctuation fix enumerated ASCII characters. Three
    // holes fell out of that: an exact-length gate could be trimmed THROUGH,
    // every Unicode punctuation class walked past the boundary rule, and a
    // credential glued to the next URL query parameter never reached the
    // classifier. All three are class problems, so all three are tested as
    // classes.

    /// I-A1 in the TIGHTENING direction. `-` and `_` are inside the Google key
    /// charset (url-safe base64) and the Google gate is an exact `n == 39`, so
    /// peeling an edge `-`/`_` drops a real 39-char key to 38 and misses the
    /// gate outright. A boundary rule may not shorten a token past a gate any
    /// more than it may lengthen one past it.
    #[test]
    fn google_keys_ending_in_a_body_char_still_hit_the_exact_gate() {
        for key in [
            concat!("AIzaSyD", "1234567890abcdefghijklmnopqrstu-"),
            concat!("AIzaSyD", "1234567890abcdefghijklmnopqrstu_"),
        ] {
            assert_eq!(key.len(), 39, "fixture must sit exactly on the gate");
            assert_every_shape(key, "Google API key");
            assert_eq!(
                detect(&format!("the key is {key} and it is live")),
                Some("Google API key"),
                "missed the bare form of: {key}"
            );
        }
    }

    /// I-A1 says punctuation, not ASCII punctuation. Smart quotes are the
    /// default macOS / Slack / Notes substitution around a pasted quoted key,
    /// so a list of ASCII characters is the wrong shape of rule: the boundary
    /// has to be a character class. Escapes, not literals, keep this file
    /// byte-clean for the em-dash sweep (I-A7).
    #[test]
    fn unicode_punctuation_bounds_a_token_like_its_ascii_twin() {
        let aws = concat!("AKIAIOSFOD", "NN7EXAMPLE");
        for s in [
            format!("the key is \u{201c}{aws}\u{201d}."),
            format!("the key is {aws}\u{2026}"),
            format!("the key is {aws}\u{2014}rotate"),
            format!("the key is {aws}\u{2013}rotate it"),
            format!("key\u{ff1a}{aws}"),
            format!("the key is {aws}\u{3002}"),
            format!("\u{ab}{aws}\u{bb}"),
            format!("the key is {aws}\u{ff01}"),
        ] {
            assert_eq!(detect(&s), Some("AWS access key id"), "missed the key in: {s}");
        }
        assert_eq!(
            detect(concat!("xoxb-", "123456789012-abcdefghijkl", "\u{2026}")),
            Some("Slack token")
        );
        assert_eq!(
            detect(concat!("AIzaSyD", "1234567890abcdefghijklmnopqrstuv", "\u{2026}")),
            Some("Google API key")
        );
        assert_eq!(
            detect("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcDEFghiJKL\u{201d}"),
            Some("JWT")
        );
    }

    /// A key pasted inside a URL is the shape the audit named, and a Google
    /// key in a maps URL is that provider's commonest leak. `&` `%` `$` `^`
    /// appear in no credential format this file knows, so splitting on them
    /// costs nothing and buys the whole query-string surface.
    #[test]
    fn credentials_inside_url_query_strings_are_caught() {
        assert_eq!(
            detect(concat!(
                "https://maps.googleapis.com/maps/api/js?key=",
                "AIzaSyD", "1234567890abcdefghijklmnopqrstuv",
                "&callback=init"
            )),
            Some("Google API key")
        );
        assert_eq!(
            detect(concat!(
                "https://api.github.com/x?token=",
                "ghp_", "1234567890abcdefghijklmnopqrstuvwxyz",
                "&page=2"
            )),
            Some("GitHub token")
        );
        assert_eq!(
            detect(concat!("key=", "AKIAIOSFOD", "NN7EXAMPLE", "&next=1")),
            Some("AWS access key id")
        );
        assert_eq!(
            detect(concat!("100%", "AKIAIOSFOD", "NN7EXAMPLE")),
            Some("AWS access key id")
        );
        assert_eq!(
            detect(concat!("$", "AKIAIOSFOD", "NN7EXAMPLE", "^rotate")),
            Some("AWS access key id")
        );
    }

    /// `_key_` is markdown emphasis too, and `-`/`_` are exempt from the first
    /// trim because they are credential body characters. The second peel is
    /// what keeps the emphasis form reachable without moving the gate.
    #[test]
    fn dash_and_underscore_wrapped_keys_are_caught() {
        assert_eq!(
            detect(concat!("the key is _", "AKIAIOSFOD", "NN7EXAMPLE", "_.")),
            Some("AWS access key id")
        );
        assert_eq!(
            detect(concat!("the key is --", "AKIAIOSFOD", "NN7EXAMPLE", "--")),
            Some("AWS access key id")
        );
        assert_eq!(
            detect(concat!("__", "AIzaSyD", "1234567890abcdefghijklmnopqrstuv", "__")),
            Some("Google API key")
        );
    }

    /// I-A2 over the second peel: it is a boundary rule as well, so a token
    /// the classifier rejected bare stays rejected after the peel.
    #[test]
    fn the_second_peel_does_not_loosen_a_gate() {
        assert_eq!(detect(concat!("_", "AKIAIOSFOD", "NN7EXAMPLEX", "_")), None);
        assert_eq!(detect(concat!("-", "AKIAIOSFOD", "NN7example", "-")), None);
        assert_eq!(
            detect(concat!("_", "ghp_", "1234567890abcdefghijklmnopqrstuvwxy", "_")),
            None
        );
        assert_eq!(
            detect(concat!("--", "AIzaSyD", "1234567890abcdefghijklmnopqrstu", "--")),
            None
        );
    }

    /// The class-based boundary reaches far more candidates, so the prose
    /// floor is re-run at Unicode edges too.
    #[test]
    fn unicode_prose_still_stores_cleanly() {
        for s in [
            "rotate the sk-key\u{2026}",
            "\u{201c}the endpoint returns a bearer token\u{201d} is prose.",
            "the token\u{2019}s value is never logged.",
            "sending xoxo-hugs-and-kisses\u{2014}every time.",
            "AIza\u{ff1a}the Google prefix, ghp_\u{ff1a}GitHub's.",
            "\u{ab}INTERNATIONALIZATION\u{bb} is a 20 letter word.",
        ] {
            assert_eq!(detect(s), None, "false positive on: {s}");
        }
    }

    /// The public write path, not just the unit. A stored secret is exactly
    /// what `admin export` -> .limpet/memory.jsonl -> git leaks, so `remember`
    /// has to refuse a period-terminated key in the body AND in the evidence
    /// command, and nothing may reach the entries table.
    #[test]
    fn remember_refuses_a_period_terminated_key() {
        let store = crate::store::Store::open_in_memory().unwrap();

        let body = concat!("prod access key is ", "AKIAIOSFOD", "NN7EXAMPLE", ".");
        let err = crate::memory::remember(
            &store, "fact", body, "explicit", None, &[], None, &[], None, false, None, false,
        )
        .expect_err("a period-terminated AWS key in the body must be refused");
        assert!(
            format!("{err:#}").contains("AWS access key id"),
            "unexpected: {err:#}"
        );

        let ev = crate::memory::Evidence {
            command: concat!("aws sts get-caller-identity --profile ", "AKIAIOSFOD", "NN7EXAMPLE", ".").into(),
            output: "ok".into(),
        };
        let err = crate::memory::remember(
            &store, "fact", "the profile answers", "explicit", None, &[], Some(&ev), &[], None,
            false, None, false,
        )
        .expect_err("a period-terminated AWS key in the evidence command must be refused");
        assert!(
            format!("{err:#}").contains("AWS access key id"),
            "unexpected: {err:#}"
        );

        let stored: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, 0, "no secret-bearing entry may enter the store");
    }

    /// The import path scans the same way: a peer's export cannot smuggle a
    /// secret in by ending the sentence.
    #[test]
    fn import_rejects_a_period_terminated_key() {
        let mut store = crate::store::Store::open_in_memory().unwrap();
        let line = concat!(
            r#"{"id":"01SECRET0000000000000000AA","kind":"insight","body":"prod key "#,
            "AKIAIOSFOD", "NN7EXAMPLE",
            r#".","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","source":"explicit","confidence":0.8,"status":"active","anchors":[],"links":[]}"#,
            "\n",
        );
        let report = store
            .import_jsonl(&mut std::io::BufReader::new(line.as_bytes()))
            .unwrap();
        assert_eq!(report.rejected, 1, "the secret-bearing line must be refused");
        assert_eq!(report.added, 0, "nothing may be applied");
    }
}
