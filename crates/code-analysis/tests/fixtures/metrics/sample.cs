class Counter
{
    private int n;

    public int Add(int a, int b)
    {
        return a + b;
    }

    public string Classify(int x, string label)
    {
        if (x > 0 && label != "")
        {
            return "pos";
        }
        else if (x < 0)
        {
            return "neg";
        }
        else
        {
            return "zero";
        }
    }

    public void Bump()
    {
        this.n++;
    }
}
