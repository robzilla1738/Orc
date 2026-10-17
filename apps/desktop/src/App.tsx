import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useSnapshot } from "./api";
import { ChatView } from "./components/ChatView";
import { InboxView } from "./components/InboxView";
import { Inspector } from "./components/Inspector";
import { LiveView } from "./components/LiveView";
import { Sidebar } from "./components/Sidebar";
import { useUi } from "./store";

const qc = new QueryClient({
  defaultOptions: { queries: { staleTime: 5_000, retry: 1 } },
});

function Shell() {
  const { data: snap, isLoading, isError } = useSnapshot();
  const { view, inspectorOpen, selectedSession } = useUi();

  return (
    <div className="app">
      {snap && <Sidebar snap={snap} />}
      <div className="main">
        <div className="content">
          {isLoading && <div className="empty"><div className="big">Connecting…</div></div>}
          {isError && (
            <div className="empty">
              <div className="big">Daemon unreachable</div>
              <div>Start <code>orcd</code> and reopen Orc — your work keeps running either way.</div>
            </div>
          )}
          {snap && (
            <>
              {view === "chat" && <ChatView />}
              {view === "live" && <LiveView snap={snap} />}
              {view === "inbox" && <InboxView snap={snap} />}
            </>
          )}
        </div>
        {snap && (inspectorOpen || selectedSession) && <Inspector snap={snap} />}
      </div>
    </div>
  );
}

export default function App() {
  return (
    <QueryClientProvider client={qc}>
      <Shell />
    </QueryClientProvider>
  );
}
