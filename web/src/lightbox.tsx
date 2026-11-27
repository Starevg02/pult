// Просмотр картинок во весь экран: стрелки листают, Esc закрывает, фокус возвращается на место.
import { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { IconArrowLeft, IconArrowRight, IconX } from "./icons";

export type LightboxItem = { src: string; name: string };

export function Lightbox({ items, index, onIndex, onClose }: { items: LightboxItem[]; index: number; onIndex: (i: number) => void; onClose: () => void }) {
  const { t } = useTranslation();
  const close = useRef<HTMLButtonElement>(null);
  const item = items[index];
  const many = items.length > 1;
  useEffect(() => {
    const back = document.activeElement as HTMLElement | null;
    close.current?.focus();
    return () => back?.focus();
  }, []);
  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
      if (many && e.key === "ArrowRight") onIndex((index + 1) % items.length);
      if (many && e.key === "ArrowLeft") onIndex((index - 1 + items.length) % items.length);
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [index, items.length, many, onClose, onIndex]);
  if (!item) return null;
  const nav = "absolute top-1/2 -translate-y-1/2 rounded-full bg-black/50 p-2 text-white hover:bg-black/70";
  return (
    <div role="dialog" aria-modal="true" aria-label={item.name} className="fixed inset-0 z-50 flex items-center justify-center bg-black/85 p-4" onClick={onClose}>
      <img src={item.src} alt={item.name} className="max-h-[88vh] max-w-full rounded-lg object-contain shadow-2xl" onClick={(e) => e.stopPropagation()} />
      <div className="absolute top-3 right-3 left-3 flex items-center gap-3 text-sm text-white" onClick={(e) => e.stopPropagation()}>
        <span className="min-w-0 flex-1 truncate">
          {item.name}
          {many && <span className="ml-2 text-white/70 tabular-nums">{`${index + 1} / ${items.length}`}</span>}
        </span>
        <a href={item.src} download={item.name} className="rounded-lg bg-white/10 px-3 py-1.5 hover:bg-white/20">
          {t("common.download")}
        </a>
        <button ref={close} type="button" className="rounded-full bg-white/10 p-2 hover:bg-white/20" aria-label={t("common.close")} onClick={onClose}>
          <IconX size={18} />
        </button>
      </div>
      {many && (
        <>
          <button
            type="button"
            className={`${nav} left-3`}
            aria-label={t("common.previous")}
            onClick={(e) => {
              e.stopPropagation();
              onIndex((index - 1 + items.length) % items.length);
            }}
          >
            <IconArrowLeft size={20} />
          </button>
          <button
            type="button"
            className={`${nav} right-3`}
            aria-label={t("common.next")}
            onClick={(e) => {
              e.stopPropagation();
              onIndex((index + 1) % items.length);
            }}
          >
            <IconArrowRight size={20} />
          </button>
        </>
      )}
    </div>
  );
}
