package com.acme;

import java.util.List;

public class Account {
    private int balance;
    private final List<Integer> history;
    private String owner;

    public Account(String owner) {
        this.owner = owner;
        this.history = new java.util.ArrayList<>();
    }

    public void deposit(int amount) {
        if (amount > 0 && amount < 1000) {
            balance += amount;
            history.add(amount);
        } else if (amount == 0) {
            return;
        } else {
            for (int i = 0; i < 3; i++) {
                if (i == amount) {
                    break;
                }
            }
        }
    }

    public String describe() {
        switch (balance) {
            case 0:
                return "empty";
            case 1:
            case 2:
                return "low";
            default:
                return "ok";
        }
    }

    public String label() {
        return owner + describe();
    }

    @Override
    public String toString() {
        String owner = "x";
        return owner;
    }

    public static Account parse(String text) {
        return text.isEmpty() ? null : new Account(text);
    }

    long positives(List<Integer> values) {
        return values.stream().filter(v -> v > 0).count();
    }
}
