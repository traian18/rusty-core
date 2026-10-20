int add(int a, int b) {
    return a + b;
}

class Counter {
public:
    Counter() : n(0) {}

    const char* classify(int x, const char* label) {
        if (x > 0 && label != nullptr) {
            return "pos";
        } else if (x < 0) {
            return "neg";
        } else {
            return "zero";
        }
    }

    void bump() {
        this->n++;
    }

private:
    int n;
};
