import type { ReactNode } from "react";
import { Icon } from "./Icon";

export type Tone = "info" | "ok" | "warn" | "danger";

interface BannerProps {
  tone: Tone;
  title?: string;
  actions?: ReactNode;
  children?: ReactNode;
}

export function Banner({ tone, title, actions, children }: BannerProps) {
  const icon = tone === "ok" ? "check" : tone === "info" ? "info" : "warn";
  return (
    <div className="banner" data-tone={tone} role={tone === "warn" || tone === "danger" ? "alert" : "status"}>
      <Icon name={icon} size={18} />
      <div className="banner-body">
        {title && <strong>{title}</strong>}
        {children}
      </div>
      {actions && <div className="banner-actions">{actions}</div>}
    </div>
  );
}
