import type { ButtonHTMLAttributes } from "react";
import { Icon, type IconName } from "./Icon";

interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: "default" | "primary" | "danger" | "quiet";
  size?: "sm" | "md" | "lg";
  icon?: IconName;
  /** Shows the button as working and ignores clicks. */
  busy?: boolean;
}

export function Button({
  variant = "default",
  size = "md",
  icon,
  busy,
  className,
  children,
  disabled,
  ...rest
}: ButtonProps) {
  const classes = ["btn", variant !== "default" && `btn-${variant}`, size !== "md" && `btn-${size}`, className]
    .filter(Boolean)
    .join(" ");
  return (
    <button type="button" className={classes} disabled={disabled || busy} aria-busy={busy || undefined} {...rest}>
      {icon && <Icon name={icon} />}
      {children}
    </button>
  );
}
