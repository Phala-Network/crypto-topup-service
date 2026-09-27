import { describe, expect, it } from "vitest";
import { formatTokenAmount, tokenAmount } from "../src/index.js";
import { quote } from "./fixtures.js";

describe("formatTokenAmount", () => {
  it("groups the whole part, drops trailing zeros, and keeps every other digit", () => {
    const rounded = quote({ amount_atomic: "1273918500000000000000" });
    expect(formatTokenAmount(rounded, "en-US")).toBe("1,273.9185");
    expect(formatTokenAmount(rounded, "de-DE")).toBe("1.273,9185");
    expect(tokenAmount(rounded)).toBe("1273.9185");

    const unrounded = quote({ amount_atomic: "12345678901234567890123" });
    expect(formatTokenAmount(unrounded, "en-US")).toBe("12,345.678901234567890123");
  });

  it("shows a whole amount without a decimal point", () => {
    const whole = quote({ amount_atomic: "80000000000000000000" });
    expect(formatTokenAmount(whole, "en-US")).toBe("80");
    expect(tokenAmount(whole)).toBe("80");
  });
});
