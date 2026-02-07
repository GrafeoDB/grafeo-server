import { useEffect, useRef } from "react";
import styles from "./ShortcutHelp.module.css";

const isMac = navigator.platform.toUpperCase().includes("MAC");
const mod = isMac ? "Cmd" : "Ctrl";

const SHORTCUTS = [
  { keys: [`${mod}+Enter`], action: "Run query" },
  { keys: [`${mod}+S`], action: "Save query" },
  { keys: ["Ctrl+\u2191"], action: "Previous history entry" },
  { keys: ["Ctrl+\u2193"], action: "Next history entry" },
  { keys: [`${mod}+Z`], action: "Undo" },
  { keys: [`${mod}+Shift+Z`], action: "Redo" },
  { keys: ["?"], action: "Toggle this help" },
];

interface ShortcutHelpProps {
  onClose: () => void;
}

export default function ShortcutHelp({ onClose }: ShortcutHelpProps) {
  const modalRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" || e.key === "?") {
        e.preventDefault();
        onClose();
      }
    };
    document.addEventListener("keydown", handleKey);
    return () => document.removeEventListener("keydown", handleKey);
  }, [onClose]);

  const handleOverlayClick = (e: React.MouseEvent) => {
    if (modalRef.current && !modalRef.current.contains(e.target as Node)) {
      onClose();
    }
  };

  return (
    <div className={styles.overlay} onClick={handleOverlayClick}>
      <div className={styles.modal} ref={modalRef}>
        <div className={styles.title}>Keyboard Shortcuts</div>
        <table className={styles.table}>
          <tbody>
            {SHORTCUTS.map((s) => (
              <tr key={s.action}>
                <td className={styles.key}>
                  {s.keys.map((k, i) => (
                    <kbd key={i}>{k}</kbd>
                  ))}
                </td>
                <td className={styles.action}>{s.action}</td>
              </tr>
            ))}
          </tbody>
        </table>
        <button className={styles.close} onClick={onClose}>
          Close
        </button>
      </div>
    </div>
  );
}
