// Части переписки: текст сообщения с разметкой, вложения (картинки сеткой, просмотр во весь экран),
// реакции, выбор эмодзи и цитата (ответ, закреплённое).
import { useEffect, useRef, useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";
import type { ChatFile, ChatQuote, ChatReaction } from "../../api/client";
import { IconFileText, IconX } from "../../icons";
import { Lightbox } from "../../lightbox";
import { formatSize } from "../helpdesk/helpdeskUtil";
import { chatFileUrl, EMOJI, REACTIONS, tokenize } from "./chatUtil";

/** Текст сообщения: ссылки, `код`, блоки кода, **жирный**, _курсив_, ~~зачёркнутый~~. */
export function MessageText({ body }: { body: string }) {
  return (
    <div className="break-words whitespace-pre-wrap">
      {tokenize(body).map((x, i) => {
        switch (x.kind) {
          case "link":
            return (
              <a key={i} href={x.href} target="_blank" rel="noreferrer noopener" className="text-accent underline underline-offset-2 hover:opacity-80">
                {x.text}
              </a>
            );
          case "code":
            return (
              <code key={i} className="rounded bg-ink/10 px-1 font-mono text-[0.85em]">
                {x.text}
              </code>
            );
          case "pre":
            return (
              <pre key={i} className="my-1 overflow-x-auto rounded-lg bg-ink/10 px-2.5 py-1.5 font-mono text-[0.85em] whitespace-pre">
                {x.text}
              </pre>
            );
          case "bold":
            return <strong key={i}>{x.text}</strong>;
          case "italic":
            return <em key={i}>{x.text}</em>;
          case "strike":
            return <s key={i}>{x.text}</s>;
          default:
            return <span key={i}>{x.text}</span>;
        }
      })}
    </div>
  );
}

/** Вложения: картинки сеткой (1–4 видно, остальное «+N»), файлы — карточками. */
export function Attachments({ files }: { files: ChatFile[] }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState<number | null>(null);
  const images = files.filter((f) => f.image);
  const other = files.filter((f) => !f.image);
  const grid = images.length === 1 ? "grid-cols-1" : "grid-cols-2";
  const shown = images.slice(0, 4);
  return (
    <div className="mt-1 space-y-1">
      {images.length > 0 && (
        <div className={`grid ${grid} gap-0.5 overflow-hidden rounded-xl`}>
          {shown.map((f, i) => (
            <button key={f.id} type="button" className="relative block cursor-zoom-in bg-sunken" onClick={() => setOpen(i)} aria-label={f.name}>
              <img src={chatFileUrl(f.id)} alt={f.name} loading="lazy" className={`w-full object-cover ${images.length === 1 ? "max-h-80 object-contain" : "aspect-square"}`} />
              {i === 3 && images.length > 4 && <span className="absolute inset-0 flex items-center justify-center bg-black/50 text-xl font-semibold text-white">+{images.length - 4}</span>}
            </button>
          ))}
        </div>
      )}
      {other.map((f) =>
        f.expired ? (
          <div key={f.id} className="flex items-center gap-2 rounded-xl bg-ink/5 px-2.5 py-2 text-muted">
            <IconFileText size={20} className="shrink-0" />
            <span className="min-w-0 flex-1">
              <span className="block truncate line-through">{f.name}</span>
              <span className="text-xs">{t("chat.expired")}</span>
            </span>
          </div>
        ) : (
          <a key={f.id} href={chatFileUrl(f.id)} download={f.name} className="flex items-center gap-2.5 rounded-xl bg-ink/5 px-2.5 py-2 hover:bg-ink/10">
            <span className="flex size-9 shrink-0 items-center justify-center rounded-full bg-accent text-paper">
              <IconFileText size={18} />
            </span>
            <span className="min-w-0 flex-1">
              <span className="block truncate font-medium">{f.name}</span>
              <span className="text-xs text-muted tabular-nums">{formatSize(f.size)}</span>
            </span>
          </a>
        ),
      )}
      {open !== null && <Lightbox items={images.map((f) => ({ src: chatFileUrl(f.id), name: f.name }))} index={open} onIndex={setOpen} onClose={() => setOpen(null)} />}
    </div>
  );
}

/** Реакции под сообщением: нажатие ставит или снимает свою. */
export function Reactions({ items, onToggle, disabled }: { items: ChatReaction[]; onToggle: (emoji: string) => void; disabled?: boolean }) {
  const { t } = useTranslation();
  if (items.length === 0) return null;
  return (
    <div className="mt-1 flex flex-wrap gap-1">
      {items.map((r) => (
        <button
          key={r.emoji}
          type="button"
          disabled={disabled}
          aria-pressed={r.mine}
          title={r.names.join(", ")}
          aria-label={t("chat.reactionLabel", { emoji: r.emoji, count: r.count, names: r.names.join(", ") })}
          onClick={() => onToggle(r.emoji)}
          className={`inline-flex items-center gap-1 rounded-full border px-1.5 py-0.5 text-xs tabular-nums transition-colors ${r.mine ? "border-accent bg-accent/15 text-accent-ink" : "border-transparent bg-ink/5 hover:bg-ink/10"}`}
        >
          <span className="text-sm leading-none">{r.emoji}</span>
          {r.count}
        </button>
      ))}
    </div>
  );
}

type PickerProps = { onPick: (emoji: string) => void; onClose: () => void; toggle: RefObject<HTMLElement | null> };

/** Общее у всплывающих выборов: щелчок мимо и Esc закрывают (кнопка-переключатель закрывает сама),
 *  после закрытия фокус возвращается на кнопку, если он потерялся. */
function usePopover(box: RefObject<HTMLElement | null>, toggle: RefObject<HTMLElement | null>, onClose: () => void, focusFirst: boolean) {
  useEffect(() => {
    if (focusFirst) box.current?.querySelector("button")?.focus();
    const away = (e: MouseEvent) => {
      const target = e.target as Node;
      if (!box.current?.contains(target) && !toggle.current?.contains(target)) onClose();
    };
    const key = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      // Esc закрывает только выбор, а не заодно ответ или правку в поле ввода.
      e.preventDefault();
      e.stopPropagation();
      onClose();
    };
    const button = toggle.current;
    document.addEventListener("mousedown", away);
    document.addEventListener("keydown", key, true);
    return () => {
      document.removeEventListener("mousedown", away);
      document.removeEventListener("keydown", key, true);
      if (!document.activeElement || document.activeElement === document.body) button?.focus();
    };
  }, [box, toggle, onClose, focusFirst]);
}

/** Ряд быстрых реакций (всплывает над сообщением или под ним). */
export function ReactionPicker({ onPick, onClose, toggle }: PickerProps) {
  const { t } = useTranslation();
  const box = useRef<HTMLDivElement>(null);
  usePopover(box, toggle, onClose, true);
  return (
    <div ref={box} role="group" aria-label={t("chat.react")} className="flex w-max max-w-[17rem] flex-wrap gap-0.5 rounded-2xl border border-line bg-paper p-1 shadow-lg">
      {REACTIONS.map((e) => (
        <button key={e} type="button" className="rounded-xl p-1 text-xl leading-none transition-transform hover:scale-125 hover:bg-hover" aria-label={e} onClick={() => onPick(e)}>
          {e}
        </button>
      ))}
    </div>
  );
}

/** Эмодзи для поля ввода. */
export function EmojiPicker({ onPick, onClose, toggle }: PickerProps) {
  const { t } = useTranslation();
  const box = useRef<HTMLDivElement>(null);
  usePopover(box, toggle, onClose, false);
  return (
    <div ref={box} role="dialog" aria-label={t("chat.emoji")} className="absolute bottom-full left-0 z-20 mb-2 grid w-72 grid-cols-8 gap-0.5 rounded-2xl border border-line bg-paper p-2 shadow-xl">
      {EMOJI.map((e) => (
        <button key={e} type="button" className="rounded-lg p-1 text-xl leading-none hover:bg-hover" aria-label={e} onClick={() => onPick(e)}>
          {e}
        </button>
      ))}
    </div>
  );
}

/** Цитата: автор и начало сообщения (ответ, закреплённое, черновик ответа). */
export function Quote({ q, onClick, onClear, label, tone = "accent" }: { q: ChatQuote; onClick?: () => void; onClear?: () => void; label?: string; tone?: "accent" | "plain" }) {
  const { t } = useTranslation();
  const text = q.preview || (q.files > 0 ? `📎 ${t("chat.files", { count: q.files })}` : "…");
  const inner = (
    <>
      <span className={`block truncate text-xs font-semibold ${tone === "accent" ? "text-accent" : ""}`}>{label ?? q.author_name}</span>
      <span className="block truncate text-xs text-muted">{label ? `${q.author_name}: ${text}` : text}</span>
    </>
  );
  return (
    <div className="flex min-w-0 items-center gap-2 border-l-2 border-accent pl-2">
      {onClick ? (
        <button type="button" className="min-w-0 flex-1 text-left" onClick={onClick}>
          {inner}
        </button>
      ) : (
        <div className="min-w-0 flex-1">{inner}</div>
      )}
      {onClear && (
        <button type="button" className="rounded-full p-1 text-muted hover:bg-hover" aria-label={t("common.cancel")} onClick={onClear}>
          <IconX size={14} />
        </button>
      )}
    </div>
  );
}
