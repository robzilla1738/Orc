import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Orbit, Unplug } from "lucide-react";
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
  const { view, selectedSession } = useUi();
  const inspected = view === "live" && snap ? snap.sessions.find((s) => s.id === selectedSession) : undefined;

  return (
    <div className="app">
      {snap ? <Sidebar snap={snap} /> : <aside className="sidebar"><div className="titlebar drag" data-tauri-drag-region /></aside>}
      <main className="main">
        <div className="content">
          {isLoading && (
            <div className="empty">
              <span className="avatar"><Orbit size={18} /></span>
              <div className="title">Connecting to orcd…</div>
            </div>
          )}
          {isError && (
            <div className="empty">
              <span className="avatar"><Unplug size={18} /></span>
              <div className="title">Daemon unreachable</div>
              <div className="body">Start <span className="mono">orcd</span> and reopen Orc. Agents keep running either way.</div>
            </div>
          )}
          {snap && view === "chat" && <ChatView />}
          {snap && view === "live" && <LiveView snap={snap} />}
          {snap && view === "inbox" && <InboxView snap={snap} />}
        </div>
        {inspected && <Inspector key={inspected.id} s={inspected} />}
      </main>
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
