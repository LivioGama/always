import type { ReactNode } from "react";
import { Icon, type IconName } from "./icons";

export type Tone = "accent" | "orange" | "teal" | "purple" | "yellow" | "green" | "red" | "blue" | "indigo";

// Literal class strings so Tailwind v4 can see them.
export const SOLID: Record<Tone, string> = {
  accent: "bg-primary-500 shadow-primary-500/30",
  orange: "bg-orange-500 shadow-orange-500/30",
  teal: "bg-teal-500 shadow-teal-500/30",
  purple: "bg-purple-500 shadow-purple-500/30",
  yellow: "bg-yellow-500 shadow-yellow-500/30",
  green: "bg-green-500 shadow-green-500/30",
  red: "bg-red-500 shadow-red-500/30",
  blue: "bg-blue-500 shadow-blue-500/30",
  indigo: "bg-indigo-500 shadow-indigo-500/30",
};
const SOFT: Record<Tone, string> = {
  accent: "bg-primary-500/12 text-primary-500",
  orange: "bg-orange-500/12 text-orange-500",
  teal: "bg-teal-500/12 text-teal-500",
  purple: "bg-purple-500/12 text-purple-500",
  yellow: "bg-yellow-500/12 text-yellow-500",
  green: "bg-green-500/12 text-green-500",
  red: "bg-red-500/12 text-red-500",
  blue: "bg-blue-500/12 text-blue-500",
  indigo: "bg-indigo-500/12 text-indigo-500",
};

/// Tinted circle with an icon (SwiftUI `Circle().fill(color.opacity(0.12))`).
export const IconCircle = ({ icon, tone, size = 40 }: { icon: IconName; tone: Tone; size?: number }) => (
  <span className={`shrink-0 inline-flex items-center justify-center rounded-full ${SOFT[tone]}`} style={{ width: size, height: size }}>
    <Icon name={icon} className={size >= 40 ? "w-[17px] h-[17px]" : "w-4 h-4"} />
  </span>
);

/// `Color.secondary.opacity(0.06)` rounded card.
export const RowCard = ({ children, ok = false, className = "", onClick, selected = false }: {
  children: ReactNode;
  ok?: boolean;
  className?: string;
  onClick?: () => void;
  selected?: boolean;
}) => (
  <div
    onClick={onClick}
    className={`flex items-center gap-3.5 rounded-xl border px-4 bg-white/[0.06] ${
      selected ? "border-primary-500/60" : ok ? "border-green-500/20" : "border-white/[0.12]"
    } ${className}`}
  >
    {children}
  </div>
);

export const RowText = ({ title, detail, bold = false }: { title: string; detail: ReactNode; bold?: boolean }) => (
  <div className="flex-1 min-w-0 flex flex-col gap-[3px]">
    <div className={`text-[15px] text-white ${bold ? "font-semibold" : "font-medium"}`}>{title}</div>
    <div className="text-xs text-surface-400">{detail}</div>
  </div>
);

export const Granted = () => <Icon name="checkCircle" className="w-[22px] h-[22px] text-green-500 shrink-0" />;
