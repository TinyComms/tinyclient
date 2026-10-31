/**
 * Every surface the desktop run walks, on Windows, Linux, and macOS, with the same names.
 * Demo scenarios use `chatd --seed-demo`. The call scenario is the live Tiny call.
 * Settings is a second window titled with its section, so this scenario only presses the rail
 * control. The section names are covered when that window is the one attached.
 */
export interface ShellScenario {
  id: string;
  summary: string;
  clicks: string[];
  expect: string[];
}

export const SHELL: readonly ShellScenario[] = [
  {
    id: "inbox",
    summary: "The Inbox lists a direct conversation from the demo.",
    clicks: ["Inbox"],
    expect: ["Yusuf Demirci"],
  },
  {
    id: "people",
    summary: "People opens from the rail and lists the demo friend.",
    clicks: ["People"],
    expect: ["Yusuf Demirci"],
  },
  {
    id: "discord",
    summary: "Discord opens Moss & Lichen and the channel that holds one of every message.",
    clicks: ["Discord", "Moss & Lichen", "#showcase"],
    expect: ["#showcase"],
  },
  {
    id: "slack",
    summary: "Slack opens the Harbor Labs workspace.",
    clicks: ["Slack"],
    expect: ["Harbor Labs"],
  },
  {
    id: "telegram",
    summary: "Telegram opens Saved Messages.",
    clicks: ["Telegram"],
    expect: ["Saved Messages"],
  },
  {
    id: "teams",
    summary: "Teams is on the rail.",
    clicks: ["Teams"],
    expect: ["Teams"],
  },
  {
    id: "signal",
    summary: "Signal is on the rail.",
    clicks: ["Signal"],
    expect: ["Signal"],
  },
  {
    id: "settings",
    summary: "Settings is on the rail.",
    clicks: ["Settings"],
    expect: [],
  },
];

export const CALL = "call";

export function scenarioById(id: string): ShellScenario | typeof CALL | undefined {
  if (id === CALL) return CALL;
  return SHELL.find((scenario) => scenario.id === id);
}
