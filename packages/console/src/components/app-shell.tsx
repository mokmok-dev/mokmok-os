import { InlineCopyText } from "@cloudflare/kumo/components/inline-copy-text";
import { Sidebar } from "@cloudflare/kumo/components/sidebar";
import { BroadcastIcon } from "@phosphor-icons/react/dist/csr/Broadcast";
import { PaperPlaneTiltIcon } from "@phosphor-icons/react/dist/csr/PaperPlaneTilt";
import { Outlet, useNavigate, useRouterState } from "@tanstack/react-router";

import { ConnectionBadge } from "./connection-badge.tsx";
import { useConsole } from "../lib/console-context.tsx";
import { useHealth } from "../lib/health.ts";

export const AppShell = () => {
  const { state } = useConsole();
  const health = useHealth();
  const navigate = useNavigate();
  const pathname = useRouterState({ select: (router) => router.location.pathname });

  const subscriber = state.subscriber ?? health.data?.subscriber ?? null;
  const socket = health.data?.socket ?? (health.isError ? "unavailable" : null);

  return (
    <Sidebar.Provider defaultOpen className="app-shell">
      <Sidebar>
        <Sidebar.Header>
          <div className="brand">
            <span className="brand-mark" aria-hidden="true">
              m
            </span>
            <span className="brand-wordmark">
              mokmok <span className="brand-product">web</span>
            </span>
          </div>
        </Sidebar.Header>

        <Sidebar.Content>
          <Sidebar.Group>
            <Sidebar.GroupLabel>Console</Sidebar.GroupLabel>
            <Sidebar.Menu>
              <Sidebar.MenuButton
                icon={BroadcastIcon}
                active={pathname === "/"}
                aria-current={pathname === "/" ? "page" : undefined}
                tooltip="Stream"
                onClick={() => void navigate({ to: "/" })}
              >
                Stream
              </Sidebar.MenuButton>
              <Sidebar.MenuButton
                icon={PaperPlaneTiltIcon}
                active={pathname === "/publish"}
                aria-current={pathname === "/publish" ? "page" : undefined}
                tooltip="Publish"
                onClick={() => void navigate({ to: "/publish" })}
              >
                Publish
              </Sidebar.MenuButton>
            </Sidebar.Menu>
          </Sidebar.Group>

          <Sidebar.Group>
            <Sidebar.GroupLabel>Subscription</Sidebar.GroupLabel>
            <div className="subscription-card">
              <div className="subscription-row">
                <span className="meta-label">Topic</span>
                <span className="meta-value mono">{state.topic}</span>
              </div>
              <div className="subscription-row">
                <span className="meta-label">Subscriber</span>
                {subscriber === null ? (
                  <span className="meta-muted">awaiting hello</span>
                ) : (
                  <InlineCopyText
                    value={subscriber}
                    variant="mono"
                    labels={{ copyAction: "Copy subscriber id", copied: "Subscriber id copied" }}
                  >
                    {subscriber.slice(0, 8)}…
                  </InlineCopyText>
                )}
              </div>
              <div className="subscription-row">
                <span className="meta-label">Connection</span>
                <ConnectionBadge phase={state.phase} />
              </div>
              <div className="subscription-row">
                <span className="meta-label">Socket</span>
                <span className="meta-value mono-sm" title={socket ?? undefined}>
                  {socket ?? "…"}
                </span>
              </div>
            </div>
          </Sidebar.Group>
        </Sidebar.Content>

        <Sidebar.Footer>
          <Sidebar.Trigger />
        </Sidebar.Footer>
      </Sidebar>

      <main className="app-main">
        <Outlet />
      </main>
    </Sidebar.Provider>
  );
};
