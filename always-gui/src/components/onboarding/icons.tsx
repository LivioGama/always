// Inline stroke icons (SF Symbol stand-ins). 24x24 viewBox, currentColor.
const PATHS = {
  waveform: "M3 12h2m3-5v10m4-14v18m4-14v10m4-5h2",
  lock: "M12 3l7 3v5c0 5-3 8-7 10-4-2-7-5-7-10V6l7-3zm-2 9l2 2 3-4",
  cpu: "M9 3v3m6-3v3M9 18v3m6-3v3M3 9h3m-3 6h3m12-6h3m-3 6h3M7 6h10a1 1 0 011 1v10a1 1 0 01-1 1H7a1 1 0 01-1-1V7a1 1 0 011-1zm3 4h4v4h-4z",
  micOff: "M12 14a3 3 0 003-3V5a3 3 0 00-6 0v6a3 3 0 003 3zm7-3a7 7 0 01-14 0m7 7v3M3 3l18 18",
  mic: "M12 14a3 3 0 003-3V5a3 3 0 00-6 0v6a3 3 0 003 3zm7-3a7 7 0 01-14 0m7 7v3",
  bulb: "M9 18h6m-5 3h4M12 3a6 6 0 00-3.5 10.9c.6.5 1 1.2 1 2.1h5c0-.9.4-1.6 1-2.1A6 6 0 0012 3z",
  seal: "M12 2l2.4 2 3.1-.3.9 3 2.7 1.6-.9 3 .9 3-2.7 1.6-.9 3-3.1-.3L12 22l-2.4-2-3.1.3-.9-3-2.7-1.6.9-3-.9-3 2.7-1.6.9-3 3.1.3L12 2zm-3 10l2 2 4-4",
  keyboard: "M3 7h18a1 1 0 011 1v8a1 1 0 01-1 1H3a1 1 0 01-1-1V8a1 1 0 011-1zm4 4h.01M11 11h.01M15 11h.01M7 14h10",
  person: "M9 11a3 3 0 100-6 3 3 0 000 6zm-6 9a6 6 0 0112 0M18 8a5 5 0 010 7m2.5-9.5a8 8 0 010 12",
  command: "M9 9V6a3 3 0 10-3 3h3zm0 0h6m-6 0v6m6-6V6a3 3 0 113 3h-3zm0 0v6m0 0h3a3 3 0 11-3 3v-3zm0 0H9m0 0v3a3 3 0 11-3-3h3z",
  ret: "M20 5v7a2 2 0 01-2 2H5m0 0l4-4m-4 4l4 4",
  clip: "M9 4h6v3H9V4zM7 5H6a2 2 0 00-2 2v12a2 2 0 002 2h12a2 2 0 002-2V7a2 2 0 00-2-2h-1",
  textCheck: "M4 6h12M4 11h8m-8 5h6m5 1l2 2 4-4",
  check: "M5 13l4 4L19 7",
  checkCircle: "M12 21a9 9 0 100-18 9 9 0 000 18zm-4-9l3 3 5-6",
  warn: "M12 8v5m0 3h.01M10.3 3.9L1.8 18a2 2 0 001.7 3h17a2 2 0 001.7-3L13.7 3.9a2 2 0 00-3.4 0z",
} as const;

export type IconName = keyof typeof PATHS;

export function Icon({ name, className = "w-4 h-4", stroke = 2 }: { name: IconName; className?: string; stroke?: number }) {
  return (
    <svg className={className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={stroke} strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      <path d={PATHS[name]} />
    </svg>
  );
}
