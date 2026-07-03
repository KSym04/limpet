<?php
/**
 * Admin dashboard page: renders the scan summary, score, and history.
 */

class Dashboard {

    private $scanner;
    private $auth;

    public function __construct( $scanner, $auth ) {
        $this->scanner = $scanner;
        $this->auth    = $auth;
    }

    /**
     * Handle the "run scan" form post.
     */
    public function handle_scan_request() {
        if ( ! $this->auth->authorize( 'feed_scan_run' ) ) {
            wp_die( esc_html__( 'Not allowed.', 'feed-scanner' ) );
        }
        $queue = new ScanQueue();
        $queue->seed( $this->count_products(), $this->batch_size() );
        wp_safe_redirect( admin_url( 'admin.php?page=feed-scanner&started=1' ) );
        exit;
    }

    /**
     * Render the score card.
     */
    public function render_score() {
        $score = $this->scanner->health_score();
        $class = 'good';
        if ( $score < 80 ) {
            $class = 'warn';
        }
        if ( $score < 50 ) {
            $class = 'bad';
        }
        printf(
            '<div class="feed-score feed-score--%s">%d</div>',
            esc_attr( $class ),
            (int) $score
        );
    }

    private function count_products() {
        return (int) wp_count_posts( 'product' )->publish;
    }

    private function batch_size() {
        return (int) apply_filters( 'feed_scanner_batch_size', 50 );
    }
}

/**
 * History table renderer for past scans.
 */
class HistoryTable {

    /**
     * Fetch recent scan history rows.
     *
     * @param int $limit Maximum rows.
     * @return array Rows as associative arrays.
     */
    public function rows( $limit = 10 ) {
        global $wpdb;
        $table = $wpdb->prefix . 'feed_scan_history';
        return $wpdb->get_results(
            $wpdb->prepare(
                "SELECT scanned_at, score, critical, warning, duration
                 FROM {$table} ORDER BY scanned_at DESC LIMIT %d",
                $limit
            ),
            ARRAY_A
        );
    }

    /**
     * Render the table markup.
     */
    public function render() {
        $rows = $this->rows();
        if ( empty( $rows ) ) {
            echo '<p>' . esc_html__( 'No scans yet.', 'feed-scanner' ) . '</p>';
            return;
        }
        echo '<table class="widefat striped feed-history">';
        echo '<thead><tr>';
        foreach ( array( 'Date', 'Score', 'Critical', 'Warnings', 'Duration' ) as $head ) {
            echo '<th>' . esc_html( $head ) . '</th>';
        }
        echo '</tr></thead><tbody>';
        foreach ( $rows as $row ) {
            printf(
                '<tr><td>%s</td><td>%d</td><td>%d</td><td>%d</td><td>%ds</td></tr>',
                esc_html( mysql2date( 'Y-m-d H:i', $row['scanned_at'] ) ),
                (int) $row['score'],
                (int) $row['critical'],
                (int) $row['warning'],
                (int) $row['duration']
            );
        }
        echo '</tbody></table>';
    }
}
