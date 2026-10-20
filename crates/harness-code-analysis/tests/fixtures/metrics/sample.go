package sample

func Add(a, b int) int {
	return a + b
}

func classify(x int, label string) string {
	if x > 0 && label != "" {
		return "pos"
	} else if x < 0 {
		return "neg"
	} else {
		return "zero"
	}
}

type Counter struct{ n int }

func (c *Counter) Bump() {
	c.n++
}
