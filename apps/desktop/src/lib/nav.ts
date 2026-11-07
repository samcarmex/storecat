export type ViewId = "search" | "drives" | "storage" | "duplicates" | "operations" | "settings" | "palette";

export const NAV: { id: ViewId; label: string }[] = [
  { id: "search", label: "Search" },
  { id: "drives", label: "Drives" },
  { id: "storage", label: "Storage" },
  { id: "duplicates", label: "Duplicates" },
  { id: "operations", label: "Operations" },
];
