"""Nightly maintenance: prune old reports and re-verify image URLs that
failed in the last scan, so transient CDN hiccups do not linger as
critical issues in the dashboard."""

import os
import time

REPORT_DIR = "/var/www/uploads/feed-reports"
MAX_AGE_DAYS = 30


def prune_reports(now: float) -> int:
    removed = 0
    cutoff = now - MAX_AGE_DAYS * 86400
    for name in os.listdir(REPORT_DIR):
        path = os.path.join(REPORT_DIR, name)
        if os.path.getmtime(path) < cutoff:
            os.remove(path)
            removed += 1
    return removed


def reverify_images(failed_urls: list) -> dict:
    results = {}
    for url in failed_urls:
        results[url] = probe(url)
        time.sleep(0.2)
    return results


def probe(url: str) -> int:
    import urllib.request

    try:
        with urllib.request.urlopen(url, timeout=5) as response:
            return response.status
    except Exception:
        return 0


if __name__ == "__main__":
    count = prune_reports(time.time())
    print(f"pruned {count} reports")


def load_failed_urls(history_path: str) -> list:
    """Read image URLs that failed in the most recent scan report."""
    import json

    with open(history_path) as f:
        history = json.load(f)
    urls = []
    for issue in history.get("issues", []):
        if issue.get("rule") == "image_unreachable":
            urls.append(issue.get("image_url"))
    return [u for u in urls if u]


def write_reverify_report(results: dict, out_path: str) -> None:
    """Persist re-verification outcomes for the dashboard to pick up."""
    import json

    recovered = {url: code for url, code in results.items() if code == 200}
    still_bad = {url: code for url, code in results.items() if code != 200}
    with open(out_path, "w") as f:
        json.dump(
            {
                "recovered": sorted(recovered),
                "still_failing": still_bad,
                "checked": len(results),
            },
            f,
            indent=2,
        )
