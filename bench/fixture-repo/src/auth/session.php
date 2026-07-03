<?php
/**
 * Request authorization for all state-changing scanner actions.
 */

class ScannerAuth {

    /**
     * Verify capability and nonce for an admin action.
     *
     * @param string $action Nonce action name.
     * @return bool True when the request may proceed.
     */
    public function authorize( $action ) {
        if ( ! current_user_can( 'manage_woocommerce' ) ) {
            return false;
        }
        $nonce = isset( $_REQUEST['_wpnonce'] ) ? sanitize_key( $_REQUEST['_wpnonce'] ) : '';
        if ( ! wp_verify_nonce( $nonce, $action ) ) {
            return false;
        }
        return true;
    }

    /**
     * Issue a signed download token for report files.
     *
     * @param string $path Report path being authorized.
     * @return string Token valid for a limited window.
     */
    public function issue_download_token( $path ) {
        $expires = time() + ( 12 * HOUR_IN_SECONDS );
        $payload = $path . '|' . $expires;
        $sig     = hash_hmac( 'sha256', $payload, wp_salt( 'auth' ) );
        return base64_encode( $payload . '|' . $sig );
    }

    /**
     * Validate a download token.
     *
     * @param string $token Token from the request.
     * @return string|false The authorized path, or false.
     */
    public function check_download_token( $token ) {
        $raw   = base64_decode( $token, true );
        $parts = explode( '|', (string) $raw );
        if ( 3 !== count( $parts ) ) {
            return false;
        }
        list( $path, $expires, $sig ) = $parts;
        if ( (int) $expires < time() ) {
            return false;
        }
        $expect = hash_hmac( 'sha256', $path . '|' . $expires, wp_salt( 'auth' ) );
        if ( ! hash_equals( $expect, $sig ) ) {
            return false;
        }
        return $path;
    }
}

/**
 * Rate limiting for scan and download endpoints.
 */
class ScannerRateLimit {

    const WINDOW_SECONDS = 60;
    const MAX_REQUESTS   = 30;

    /**
     * Check and consume one request slot for the current user.
     *
     * @param string $bucket Endpoint bucket name.
     * @return bool True when the request is within limits.
     */
    public function allow( $bucket ) {
        $key   = 'feed_rl_' . $bucket . '_' . get_current_user_id();
        $count = (int) get_transient( $key );
        if ( $count >= self::MAX_REQUESTS ) {
            return false;
        }
        set_transient( $key, $count + 1, self::WINDOW_SECONDS );
        return true;
    }

    /**
     * Remaining slots for response headers.
     *
     * @param string $bucket Endpoint bucket name.
     * @return int Remaining requests in the window.
     */
    public function remaining( $bucket ) {
        $key   = 'feed_rl_' . $bucket . '_' . get_current_user_id();
        $count = (int) get_transient( $key );
        return max( 0, self::MAX_REQUESTS - $count );
    }
}
