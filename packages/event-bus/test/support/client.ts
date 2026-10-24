import { WebSocket } from "ws";

export interface TestClient {
  readonly socket: WebSocket;
  readonly inbox: ReadonlyArray<unknown>;
  readonly take: (match: (frame: Record<string, unknown>) => boolean) => Promise<unknown>;
  readonly waitClosed: () => Promise<number>;
  readonly send: (frame: Record<string, unknown>) => void;
  readonly stop: () => void;
}

const delay = (millis: number): Promise<void> =>
  new Promise((resolve) => {
    setTimeout(resolve, millis);
  });

export const connect = async (socketPath: string, timeoutMillis = 3000): Promise<TestClient> => {
  const socket = new WebSocket(`ws+unix://${socketPath}:/`);
  const inbox: Array<Record<string, unknown>> = [];
  let closed = -1;
  socket.on("message", (data) => {
    inbox.push(JSON.parse(String(data)) as Record<string, unknown>);
  });
  socket.on("close", (code) => {
    closed = code;
  });
  await new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("client did not open")), timeoutMillis);
    socket.once("open", () => {
      clearTimeout(timer);
      resolve();
    });
    socket.once("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
  });

  const take = async (
    match: (frame: Record<string, unknown>) => boolean,
    waitMillis = timeoutMillis,
  ): Promise<unknown> => {
    const deadline = Date.now() + waitMillis;
    while (Date.now() < deadline) {
      const index = inbox.findIndex(match);
      if (index >= 0) {
        return inbox.splice(index, 1)[0];
      }
      await delay(5);
    }
    throw new Error(`no matching frame; inbox holds ${JSON.stringify(inbox)}`);
  };

  const waitClosed = async (waitMillis = timeoutMillis): Promise<number> => {
    const deadline = Date.now() + waitMillis;
    while (Date.now() < deadline) {
      if (closed >= 0) return closed;
      await delay(5);
    }
    throw new Error("client was never closed");
  };

  return {
    socket,
    inbox,
    take,
    waitClosed,
    send: (frame) => {
      socket.send(JSON.stringify(frame));
    },
    stop: () => {
      socket.close();
    },
  };
};

export const takeOp = (op: string) => (frame: Record<string, unknown>) => frame["op"] === op;
