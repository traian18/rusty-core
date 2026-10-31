class Counter
  def initialize
    @n = 0
  end

  def classify(x, label)
    if x > 0 && label != ""
      "pos"
    elsif x < 0
      "neg"
    else
      "zero"
    end
  end

  def bump
    @n += 1
  end
end
