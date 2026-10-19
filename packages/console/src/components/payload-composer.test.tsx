// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { MAX_PAYLOAD_BYTES } from "@mokmok/web-consumer";

import "../test/polyfills.ts";
import { PayloadComposer } from "../components/payload-composer.tsx";

afterEach(cleanup);

const publishButton = (): HTMLButtonElement =>
  screen.getByRole("button", { name: /Publish event/ }) as HTMLButtonElement;

const payloadField = (): HTMLTextAreaElement =>
  screen.getByLabelText("Payload (JSON)") as HTMLTextAreaElement;

describe("PayloadComposer", () => {
  it("keeps publishing disabled until the text is a valid JSON value", () => {
    render(<PayloadComposer onPublish={() => undefined} publishing={false} lastRequestId={null} />);

    expect(publishButton().disabled).toBe(true);

    fireEvent.change(payloadField(), { target: { value: "{" } });
    expect(publishButton().disabled).toBe(true);
    expect(screen.getAllByText("the payload is not valid JSON").length).toBeGreaterThan(0);

    fireEvent.change(payloadField(), { target: { value: '{"kind":"demo"}' } });
    expect(publishButton().disabled).toBe(false);
    expect(screen.getAllByText("Valid JSON — ready to publish.").length).toBeGreaterThan(0);
  });

  it("fills the sample payload and publishes the parsed value", () => {
    const onPublish = vi.fn();
    render(<PayloadComposer onPublish={onPublish} publishing={false} lastRequestId={null} />);

    fireEvent.click(screen.getByRole("button", { name: /use a sample payload/i }));
    expect(payloadField().value).toContain('"kind": "console.demo"');

    fireEvent.click(publishButton());
    expect(onPublish).toHaveBeenCalledTimes(1);
    expect(onPublish).toHaveBeenCalledWith(
      expect.objectContaining({ kind: "console.demo", level: "info" }),
    );
  });

  it("reports UTF-8 size and rejects a payload over MAX_PAYLOAD_BYTES", () => {
    render(<PayloadComposer onPublish={() => undefined} publishing={false} lastRequestId={null} />);

    const oversized = JSON.stringify("x".repeat(MAX_PAYLOAD_BYTES));
    fireEvent.change(payloadField(), { target: { value: oversized } });

    expect(
      screen.getAllByText(`the payload is larger than ${MAX_PAYLOAD_BYTES} bytes`).length,
    ).toBeGreaterThan(0);
    expect(publishButton().disabled).toBe(true);
  });

  it("shows the last request id and disables the button while publishing", () => {
    render(
      <PayloadComposer
        onPublish={() => undefined}
        publishing
        lastRequestId="11111111-2222-4333-8444-555555555555"
      />,
    );

    expect(screen.getAllByText("11111111-2222-4333-8444-555555555555").length).toBeGreaterThan(0);
    expect(publishButton().disabled).toBe(true);
  });

  it("does not publish while the payload is empty and clears the editor", async () => {
    const onPublish = vi.fn();
    render(<PayloadComposer onPublish={onPublish} publishing={false} lastRequestId={null} />);

    fireEvent.click(screen.getByRole("button", { name: "Clear" }));
    expect(payloadField().value).toBe("");
    expect(publishButton().disabled).toBe(true);

    fireEvent.change(payloadField(), { target: { value: "42" } });
    await waitFor(() => expect(publishButton().disabled).toBe(false));
    expect(onPublish).not.toHaveBeenCalled();
  });
});
