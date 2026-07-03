<?php
/**
 * Persistent scan queue. Chunks the catalog into batches and processes one
 * batch per request, so shared hosts with strict execution limits can
 * finish a full catalog scan across many requests.
 */

class ScanQueue {

    private $store_key = 'feed_scan_queue';

    /**
     * Enqueue the whole catalog as batch descriptors.
     *
     * @param int $total_products Number of products in the catalog.
     * @param int $batch_size     Products per batch.
     */
    public function seed( $total_products, $batch_size ) {
        $batches = (int) ceil( $total_products / $batch_size );
        $queue   = array();
        for ( $i = 0; $i < $batches; $i++ ) {
            $queue[] = array(
                'offset' => $i * $batch_size,
                'limit'  => $batch_size,
                'state'  => 'pending',
            );
        }
        update_option( $this->store_key, $queue, false );
    }

    /**
     * Pop the next pending batch, mark it running, return it.
     *
     * @return array|null Batch descriptor or null when drained.
     */
    public function next() {
        $queue = get_option( $this->store_key, array() );
        foreach ( $queue as $i => $batch ) {
            if ( 'pending' === $batch['state'] ) {
                $queue[ $i ]['state'] = 'running';
                update_option( $this->store_key, $queue, false );
                return $queue[ $i ];
            }
        }
        return null;
    }

    /**
     * Mark a batch done by offset.
     *
     * @param int $offset Batch offset to complete.
     */
    public function complete( $offset ) {
        $queue = get_option( $this->store_key, array() );
        foreach ( $queue as $i => $batch ) {
            if ( $batch['offset'] === $offset ) {
                $queue[ $i ]['state'] = 'done';
            }
        }
        update_option( $this->store_key, $queue, false );
    }
}

/**
 * Companion helpers for queue introspection used by the REST layer.
 */
class ScanQueueStatus {

    /**
     * Progress summary across all batches.
     *
     * @return array Done, running, pending, and total counts.
     */
    public function progress() {
        $queue  = get_option( 'feed_scan_queue', array() );
        $counts = array( 'done' => 0, 'running' => 0, 'pending' => 0 );
        foreach ( $queue as $batch ) {
            $counts[ $batch['state'] ]++;
        }
        $counts['total'] = count( $queue );
        return $counts;
    }

    /**
     * Recover batches stuck in running state after a fatal.
     *
     * Batches older than ten minutes in running state flip back to
     * pending so the next request retries them.
     *
     * @return int Number of recovered batches.
     */
    public function recover_stuck() {
        $queue     = get_option( 'feed_scan_queue', array() );
        $recovered = 0;
        foreach ( $queue as $i => $batch ) {
            if ( 'running' !== $batch['state'] ) {
                continue;
            }
            $started = isset( $batch['started_at'] ) ? (int) $batch['started_at'] : 0;
            if ( ( time() - $started ) > 600 ) {
                $queue[ $i ]['state'] = 'pending';
                $recovered++;
            }
        }
        if ( $recovered > 0 ) {
            update_option( 'feed_scan_queue', $queue, false );
        }
        return $recovered;
    }

    /**
     * Drop the queue entirely, used by the reset action and uninstall.
     */
    public function clear() {
        delete_option( 'feed_scan_queue' );
    }
}
