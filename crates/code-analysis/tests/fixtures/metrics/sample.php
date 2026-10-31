<?php
function add($a, $b) {
    return $a + $b;
}

class Counter {
    private $n = 0;

    public function classify(int $x, string $label) {
        if ($x > 0 && $label != "") {
            return "pos";
        } elseif ($x < 0) {
            return "neg";
        } else {
            return "zero";
        }
    }

    public function bump() {
        $this->n++;
    }
}
