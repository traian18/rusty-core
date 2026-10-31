package com.acme

// Totals and classification.
fun classify(n: Int, flag: Boolean): String {
    if (n > 0 && flag) {
        return "positive"
    } else if (n < 0 || !flag) {
        return "negative"
    } else {
        for (i in 0..n) {
            if (i == 3) {
                return "three"
            }
        }
    }
    return "zero"
}

class Account(private val owner: String, var balance: Int) {
    private var history: MutableList<Int> = mutableListOf()

    fun deposit(amount: Int) {
        if (amount > 0) {
            balance += amount
            history.add(amount)
        }
    }

    fun report(): String = owner + describe()

    private fun describe(): String {
        return when (balance) {
            0 -> "empty"
            1, 2 -> "low"
            else -> "ok"
        }
    }

    override fun toString(): String {
        val owner = "shadowed"
        return owner
    }

    constructor(owner: String) : this(owner, 0)

    companion object {
        fun parse(text: String): Account = Account(text)
    }
}

/** Doubles the positive values. */
fun total(values: List<Int>): Int {
    return values.filter { it > 0 }.sumOf { v -> v * 2 }
}
