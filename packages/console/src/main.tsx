import "@cloudflare/kumo/styles/standalone";
import "./styles/app.css";

import { Toasty } from "@cloudflare/kumo/components/toast";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { createRoot } from "react-dom/client";

import { ConsoleProvider } from "./lib/console-context.tsx";
import { router } from "./router.tsx";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      refetchOnWindowFocus: true,
      retry: 0,
    },
  },
});

const container = document.getElementById("root");
if (container === null) throw new Error("The console root element is missing");

// No StrictMode: it double-invokes effects in development, which would briefly open a second
// browser WebSocket against the bridge's one-client-at-a-time stream.
createRoot(container).render(
  <QueryClientProvider client={queryClient}>
    <Toasty>
      <ConsoleProvider>
        <RouterProvider router={router} />
      </ConsoleProvider>
    </Toasty>
  </QueryClientProvider>,
);
