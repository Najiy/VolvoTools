import { useEffect, useState } from "react";

export type Theme = "system" | "dark" | "light";

const KEY = "volvotools.theme";

function stored(): Theme {
  try {
    const value = localStorage.getItem(KEY);
    return value === "dark" || value === "light" ? value : "system";
  } catch {
    return "system";
  }
}

/** The chosen theme, applied as `data-theme` on the root. "system" follows the OS. */
export function useTheme(): [Theme, (theme: Theme) => void] {
  const [theme, setTheme] = useState<Theme>(stored);
  useEffect(() => {
    const root = document.documentElement;
    if (theme === "system") root.removeAttribute("data-theme");
    else root.setAttribute("data-theme", theme);
    try {
      localStorage.setItem(KEY, theme);
    } catch {
      // Storage can be unavailable; the choice then only lasts for this session.
    }
  }, [theme]);
  return [theme, setTheme];
}
