import type { ConsumerMessage } from "@mokmok/web-consumer";
import type { BridgeServerFrame } from "@mokmok/web-protocol";

export const frameToMessage = (frame: BridgeServerFrame): ConsumerMessage => {
  switch (frame.op) {
    case "hello":
      return { type: "hello", topic: frame.topic, subscriber: frame.subscriber };
    case "status":
      return { type: "status", connection: frame.connection, detail: frame.detail };
    case "batch":
      return { type: "batch", frame };
    case "acked":
      return { type: "acked", through: frame.through };
    case "published":
      return { type: "published", frame };
    case "publish_failed":
      return { type: "publish_failed", frame };
  }
};
