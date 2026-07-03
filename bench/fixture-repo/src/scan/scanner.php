<?php
/**
 * Product feed scanner. Walks the catalog in batches and applies every
 * registered rule to each product, collecting issues for the report.
 */

use App\Scan\Rules;

class FeedScanner {

    private $rules;
    private $batch_size;
    private $issues;

    public function __construct( $rules, $batch_size ) {
        $this->rules      = $rules;
        $this->batch_size = $batch_size;
        $this->issues     = array();
    }

    /**
     * Scan one batch of products.
     *
     * @param array $products Product objects for this batch.
     * @return array Issues found in this batch.
     */
    public function scan_batch( $products ) {
        $found = array();
        foreach ( $products as $product ) {
            if ( 'draft' === $product->status ) {
                continue;
            }
            $result = $this->check_product( $product );
            if ( ! empty( $result ) ) {
                $found = array_merge( $found, $result );
            }
        }
        $this->issues = array_merge( $this->issues, $found );
        return $found;
    }

    /**
     * Apply every rule to a single product.
     *
     * @param object $product The product under inspection.
     * @return array Issue descriptors, possibly empty.
     */
    public function check_product( $product ) {
        $found = array();
        foreach ( $this->rules as $rule ) {
            $issue = call_user_func( $rule, $product );
            if ( false !== $issue ) {
                $found[] = array(
                    'product_id' => $product->id,
                    'rule'       => $issue['rule'],
                    'level'      => $issue['level'],
                    'message'    => $issue['message'],
                );
            }
        }
        return $found;
    }

    /**
     * Health score for the last scan: 100 minus weighted issue counts.
     *
     * @return int Score between 0 and 100.
     */
    public function health_score() {
        $critical = 0;
        $warning  = 0;
        foreach ( $this->issues as $issue ) {
            if ( 'critical' === $issue['level'] ) {
                $critical++;
            } else {
                $warning++;
            }
        }
        $score = 100 - ( $critical * 10 ) - ( $warning * 2 );
        return max( 0, $score );
    }

    /**
     * Summarize issues grouped by rule for the dashboard table.
     *
     * @return array Map of rule name to counts per level.
     */
    public function summarize_by_rule() {
        $summary = array();
        foreach ( $this->issues as $issue ) {
            $rule = $issue['rule'];
            if ( ! isset( $summary[ $rule ] ) ) {
                $summary[ $rule ] = array( 'critical' => 0, 'warning' => 0 );
            }
            $summary[ $rule ][ $issue['level'] ]++;
        }
        ksort( $summary );
        return $summary;
    }

    /**
     * Persist the finished scan to the history table.
     *
     * @param int $duration_seconds Wall time of the whole scan.
     * @return int Insert id of the history row.
     */
    public function persist_history( $duration_seconds ) {
        global $wpdb;
        $table = $wpdb->prefix . 'feed_scan_history';
        $wpdb->insert(
            $table,
            array(
                'scanned_at' => current_time( 'mysql', true ),
                'score'      => $this->health_score(),
                'critical'   => $this->count_level( 'critical' ),
                'warning'    => $this->count_level( 'warning' ),
                'duration'   => (int) $duration_seconds,
                'issues'     => wp_json_encode( $this->issues ),
            ),
            array( '%s', '%d', '%d', '%d', '%d', '%s' )
        );
        return (int) $wpdb->insert_id;
    }

    /**
     * Count issues at a given level in the current scan.
     *
     * @param string $level Either critical or warning.
     * @return int Number of issues at that level.
     */
    public function count_level( $level ) {
        $count = 0;
        foreach ( $this->issues as $issue ) {
            if ( $issue['level'] === $level ) {
                $count++;
            }
        }
        return $count;
    }

    /**
     * Compare this scan against the previous history row.
     *
     * @return array Delta of critical and warning counts.
     */
    public function delta_since_last() {
        global $wpdb;
        $table = $wpdb->prefix . 'feed_scan_history';
        $row   = $wpdb->get_row(
            "SELECT critical, warning FROM {$table} ORDER BY scanned_at DESC LIMIT 1 OFFSET 1",
            ARRAY_A
        );
        if ( null === $row ) {
            return array( 'critical' => 0, 'warning' => 0, 'first_scan' => true );
        }
        return array(
            'critical'   => $this->count_level( 'critical' ) - (int) $row['critical'],
            'warning'    => $this->count_level( 'warning' ) - (int) $row['warning'],
            'first_scan' => false,
        );
    }

    /**
     * Reset collected issues between runs.
     */
    public function reset() {
        $this->issues = array();
    }

    /**
     * Expose collected issues for the exporter.
     *
     * @return array All issues collected so far.
     */
    public function get_issues() {
        return $this->issues;
    }
}
}
