// UI-only state — runtime truth lives in the daemon, mirrored by the
// snapshot/event query layer in api.ts.
import { create } from "zustand";

export type View = "chat" | "live" | "inbox";

interface UiState {
  view: View;
  selectedSession: string | null;
  setView: (v: View) => void;
  selectSession: (id: string | null) => void;
}

export const useUi = create<UiState>((set) => ({
  view: "chat",
  selectedSession: null,
  setView: (view) => set({ view }),
  selectSession: (selectedSession) => set({ selectedSession }),
}));
