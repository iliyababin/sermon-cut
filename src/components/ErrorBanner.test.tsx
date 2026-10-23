import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { ErrorBanner } from "./ErrorBanner";

afterEach(cleanup);

describe("ErrorBanner", () => {
  it("is an alert with the message and only the actions given", () => {
    const retry = vi.fn();
    const dismiss = vi.fn();
    render(<ErrorBanner message="The disk is full." onRetry={retry} onDismiss={dismiss} />);
    expect(screen.getByRole("alert").textContent).toContain("The disk is full.");
    expect(screen.queryByText("Show leftovers")).toBeNull();
    fireEvent.click(screen.getByText("Try again"));
    fireEvent.click(screen.getByText("Dismiss"));
    expect(retry).toHaveBeenCalledTimes(1);
    expect(dismiss).toHaveBeenCalledTimes(1);
  });
});
