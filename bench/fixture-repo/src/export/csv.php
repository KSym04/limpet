<?php
/**
 * CSV report exporter. Writes scan issues to a downloadable file.
 */

class CsvExporter {

    private $delimiter = ';';

    /**
     * Export issues to a CSV file under the uploads directory.
     *
     * @param array  $issues   Issue rows from the scanner.
     * @param string $filename Target file name, no path.
     * @return string Absolute path of the written file.
     */
    public function export( $issues, $filename ) {
        $dir  = wp_upload_dir();
        $path = trailingslashit( $dir['basedir'] ) . 'feed-reports/' . $filename;
        wp_mkdir_p( dirname( $path ) );
        $handle = fopen( $path, 'w' );
        fwrite( $handle, "\xEF\xBB\xBF" );
        $this->write_row( $handle, array( 'Product', 'Rule', 'Level', 'Message' ) );
        foreach ( $issues as $issue ) {
            $this->write_row( $handle, array(
                $issue['product_id'],
                $issue['rule'],
                $issue['level'],
                $issue['message'],
            ) );
        }
        fclose( $handle );
        return $path;
    }

    /**
     * Write one row, quoting every field that contains the delimiter.
     *
     * @param resource $handle Open file handle.
     * @param array    $fields Field values.
     */
    public function write_row( $handle, $fields ) {
        $quoted = array();
        foreach ( $fields as $field ) {
            $field = (string) $field;
            if ( false !== strpos( $field, $this->delimiter ) || false !== strpos( $field, '"' ) ) {
                $field = '"' . str_replace( '"', '""', $field ) . '"';
            }
            $quoted[] = $field;
        }
        fwrite( $handle, implode( $this->delimiter, $quoted ) . "\r\n" );
    }
}

/**
 * Report file housekeeping shared by the exporter and the REST layer.
 */
class ReportFiles {

    /**
     * List existing report files newest first.
     *
     * @return array Absolute paths.
     */
    public function list_reports() {
        $dir   = wp_upload_dir();
        $base  = trailingslashit( $dir['basedir'] ) . 'feed-reports/';
        $files = glob( $base . '*.csv' );
        if ( false === $files ) {
            return array();
        }
        usort( $files, function ( $a, $b ) {
            return filemtime( $b ) - filemtime( $a );
        } );
        return $files;
    }

    /**
     * Build the public URL for a report path, going through the signed
     * download endpoint rather than exposing the uploads path directly.
     *
     * @param string $path Absolute report path.
     * @param string $token Signed token from ScannerAuth.
     * @return string Download URL.
     */
    public function download_url( $path, $token ) {
        return add_query_arg(
            array(
                'action' => 'feed_scanner_download',
                'file'   => rawurlencode( basename( $path ) ),
                'token'  => rawurlencode( $token ),
            ),
            admin_url( 'admin-post.php' )
        );
    }

    /**
     * Stream a report to the browser with download headers.
     *
     * @param string $path Absolute report path, already authorized.
     */
    public function stream( $path ) {
        if ( ! file_exists( $path ) ) {
            wp_die( esc_html__( 'Report not found.', 'feed-scanner' ), 404 );
        }
        nocache_headers();
        header( 'Content-Type: text/csv; charset=UTF-8' );
        header( 'Content-Disposition: attachment; filename="' . basename( $path ) . '"' );
        header( 'Content-Length: ' . filesize( $path ) );
        readfile( $path );
        exit;
    }

    /**
     * Delete reports older than the retention window.
     *
     * @param int $days Retention in days.
     * @return int Number of files removed.
     */
    public function prune( $days ) {
        $removed = 0;
        $cutoff  = time() - ( $days * DAY_IN_SECONDS );
        foreach ( $this->list_reports() as $path ) {
            if ( filemtime( $path ) < $cutoff ) {
                wp_delete_file( $path );
                $removed++;
            }
        }
        return $removed;
    }
}
