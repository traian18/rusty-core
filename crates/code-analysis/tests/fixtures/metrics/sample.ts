// Utilities for orders.
import { load } from "./store";

/**
 * Totals an order.
 */
export function total(items: number[], discount?: number): number {
  let sum = 0;
  for (const item of items) {
    if (item > 0 && item < 100) {
      sum += item;
    } else if (item >= 100) {
      sum += item * 0.9;
    }
  }
  return discount ? sum - discount : sum;
}

export class Cart {
  private items: number[] = [];
  private owner = "";

  add(this: Cart, item: number): void {
    this.items.push(item);
  }

  count(): number {
    return this.items.length;
  }

  rename(name: string) {
    this.owner = name;
  }

  describe = () => {
    return `${this.owner} has ${this.count()} items`;
  };

  static empty(): Cart {
    return new Cart();
  }
}

describe("cart", () => {
  it("adds", () => {
    switch (load()) {
      case 1:
        break;
      case 2:
        break;
      default:
        break;
    }
  });
});
