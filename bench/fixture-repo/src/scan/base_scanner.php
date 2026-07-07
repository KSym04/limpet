<?php
interface Scannable {
    public function scan_batch($items);
}
abstract class BaseScanner implements Scannable {
    protected function health_score($crit, $warn) { return max(0, 100 - 10*$crit - 2*$warn); }
}
