import { createRootRoute, createRoute, createRouter } from "@tanstack/react-router";

import { AppShell } from "./components/app-shell.tsx";
import { PublishPage } from "./routes/publish-page.tsx";
import { StreamPage } from "./routes/stream-page.tsx";

const rootRoute = createRootRoute({ component: AppShell });

const streamRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/",
  component: StreamPage,
});

const publishRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/publish",
  component: PublishPage,
});

const routeTree = rootRoute.addChildren([streamRoute, publishRoute]);

export const router = createRouter({ routeTree, defaultPreload: "intent" });

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
