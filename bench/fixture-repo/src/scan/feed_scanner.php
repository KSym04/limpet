<?php
class FeedScanner extends BaseScanner {
    public function scan_batch($items) { return $this->health_score(1, 2); }
}
