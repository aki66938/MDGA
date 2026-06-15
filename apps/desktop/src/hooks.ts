// 自洽 hook 集合（行为敏感解耦增量 1）：从 App() 纯搬移出来的低耦合状态/effect/函数。
// 铁律：逻辑/时序/依赖数组与原 App 内完全一致，仅做位置迁移。
import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { Toast, UpdateState } from "./types";

/**
 * 主题 hook（迁自 App）：启动时从 localStorage 恢复（默认跟随系统），并同步到 <html data-theme>。
 * 返回 [theme, setTheme]，调用方式与原 useState 完全一致（含函数式 setTheme）。
 */
export function useTheme(): [
  "light" | "dark",
  React.Dispatch<React.SetStateAction<"light" | "dark">>,
] {
  const [theme, setTheme] = useState<"light" | "dark">("light");
  // 主题：启动时从 localStorage 恢复（默认跟随系统），并同步到 <html data-theme>
  useEffect(() => {
    const saved = localStorage.getItem("mdga.theme") as "light" | "dark" | null;
    const initial =
      saved ?? (window.matchMedia?.("(prefers-color-scheme: dark)").matches ? "dark" : "light");
    setTheme(initial);
  }, []);
  useEffect(() => {
    document.documentElement.setAttribute("data-theme", theme);
    localStorage.setItem("mdga.theme", theme);
  }, [theme]);
  return [theme, setTheme];
}

/**
 * 全局 toast hook（迁自 App，Plan20 🔴2）：右下角堆叠通知。
 * 返回 { toasts, pushToast, dismissToast }，调用点保持不变。
 */
export function useToasts(): {
  toasts: Toast[];
  pushToast: (kind: Toast["kind"], text: string) => void;
  dismissToast: (id: number) => void;
} {
  const [toasts, setToasts] = useState<Toast[]>([]);

  /** 弹出全局 toast（Plan20 🔴2）：右下角堆叠，数秒后自动消失；error 略长于 info。 */
  function pushToast(kind: Toast["kind"], text: string) {
    const id = Date.now() + Math.random();
    setToasts((prev) => [...prev, { id, kind, text }]);
    const ttl = kind === "error" ? 6000 : 4000;
    window.setTimeout(() => {
      setToasts((prev) => prev.filter((t) => t.id !== id));
    }, ttl);
  }

  /** 手动关闭某条 toast。 */
  function dismissToast(id: number) {
    setToasts((prev) => prev.filter((t) => t.id !== id));
  }

  return { toasts, pushToast, dismissToast };
}

/**
 * 更新检查 hook（迁自 App）：挂载后延迟 check_update + 监听 update-progress；提供 handleInstallUpdate。
 * 返回 { update, setUpdate, handleInstallUpdate }；setUpdate 供侧栏「稍后/关闭」按钮置回 idle。
 */
export function useUpdate(): {
  update: UpdateState;
  setUpdate: React.Dispatch<React.SetStateAction<UpdateState>>;
  handleInstallUpdate: () => Promise<void>;
} {
  const [update, setUpdate] = useState<UpdateState>({ status: "idle" });

  useEffect(() => {
    const timer = setTimeout(() => {
      invoke<string | null>("check_update")
        .then((v) => { if (v) setUpdate({ status: "available", version: v }); })
        .catch(() => {});
    }, 3000);
    const unlistenProgress = listen<number>("update-progress", (e) => {
      setUpdate({ status: "downloading", progress: e.payload });
    });
    return () => {
      clearTimeout(timer);
      unlistenProgress.then((fn) => fn());
    };
  }, []);

  async function handleInstallUpdate() {
    setUpdate({ status: "downloading", progress: 0 });
    try {
      await invoke("install_update");
    } catch (err) {
      setUpdate({ status: "error", message: String(err) });
    }
  }

  return { update, setUpdate, handleInstallUpdate };
}

/**
 * 全局快捷键 hook（迁自 App，Plan27 #3a）：Ctrl/Cmd+N 新对话、Ctrl/Cmd+K 命令面板、Ctrl/Cmd+, 设置。
 * 用 ref 镜像回调，规避闭包陈旧；只注册一次。调用点把对应动作以 opts 回调传入。
 */
export function useKeyboardShortcuts(opts: {
  onNewConversation: () => void;
  onOpenPalette: () => void;
  onOpenSettings: () => void;
}): void {
  const shortcutState = useRef(opts);
  shortcutState.current = opts;
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (!(e.metaKey || e.ctrlKey)) return;
      const s = shortcutState.current;
      const key = e.key.toLowerCase();
      if (key === "n") {
        e.preventDefault();
        void s.onNewConversation();
      } else if (key === "k") {
        e.preventDefault();
        s.onOpenPalette();
      } else if (e.key === ",") {
        e.preventDefault();
        void s.onOpenSettings();
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);
}
