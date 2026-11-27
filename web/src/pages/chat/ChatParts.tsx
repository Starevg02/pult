// Части переписки: текст сообщения с разметкой, вложения (картинки мозаикой, документы карточками,
// просмотр во весь экран), реакции, панель «Эмодзи / GIF» и цитата (ответ, закреплённое).
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState, type ReactNode, type RefObject } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type ChatFile, type ChatGif, type ChatKlipyGif, type ChatQuote, type ChatReaction } from "../../api/client";
import { IconFileText, IconPlus, IconSearch, IconX } from "../../icons";
import { Lightbox } from "../../lightbox";
import { toast } from "../../toast";
import { formatSize, postFile } from "../helpdesk/helpdeskUtil";
import { chatFileUrl, REACTIONS, tokenize, useKlipy } from "./chatUtil";
import { KlipyTab } from "./klipy";
import { EMOJI_GROUPS, recentEmoji, rememberEmoji, searchEmoji } from "./emoji";

/** Текст сообщения: ссылки, `код`, блоки кода, **жирный**, _курсив_, ~~зачёркнутый~~. */
/** Текст сообщения. `tail` — в конце последней строки (место под время, как в мессенджерах). */
export function MessageText({ body, tail }: { body: string; tail?: ReactNode }) {
  return (
    <div className="break-words whitespace-pre-wrap">
      {tokenize(body).map((x, i) => {
        switch (x.kind) {
          case "link":
            return (
              <a key={i} href={x.href} target="_blank" rel="noreferrer noopener" className="text-accent underline decoration-1 underline-offset-2 hover:opacity-80">
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
      {tail}
    </div>
  );
}

const isGif = (f: ChatFile) => f.mime === "image/gif";

/** Картинки мозаикой: одна — целиком, две — рядом, три — большая и две справа, дальше — 2×2 и «+N».
 *  Размеры ячеек заданы заранее: лента не прыгает, пока картинки догружаются. */
export function Media({ files, overlay }: { files: ChatFile[]; overlay?: ReactNode }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState<number | null>(null);
  const shown = files.slice(0, 4);
  const n = files.length;
  const layout = n === 1 ? "grid-cols-1" : n === 3 ? "grid-cols-[2fr_1fr] grid-rows-2" : "grid-cols-2";
  const cell = (i: number) =>
    n === 1 ? "aspect-[4/3] max-h-[22rem]" : n === 2 ? "aspect-[3/4]" : n === 3 ? (i === 0 ? "row-span-2 h-full min-h-56" : "aspect-square") : "aspect-square";
  return (
    <div className="relative">
      <div className={`grid ${layout} gap-0.5`}>
        {shown.map((f, i) => (
          <button key={f.id} type="button" className={`relative block w-full cursor-zoom-in overflow-hidden bg-ink/10 ${cell(i)}`} onClick={() => setOpen(i)} aria-label={f.name}>
            <img src={chatFileUrl(f.id)} alt={f.name} loading="lazy" className="size-full object-cover" />
            {isGif(f) && <span className="absolute top-1.5 left-1.5 rounded bg-black/50 px-1 text-[10px] font-bold tracking-wide text-white">GIF</span>}
            {i === 3 && n > 4 && <span className="absolute inset-0 flex items-center justify-center bg-black/50 text-2xl font-semibold text-white">+{n - 4}</span>}
          </button>
        ))}
      </div>
      {overlay && <div className="pointer-events-none absolute right-1.5 bottom-1.5">{overlay}</div>}
      {open !== null && <Lightbox items={files.map((f) => ({ src: chatFileUrl(f.id), name: f.name }))} index={open} onIndex={setOpen} onClose={() => setOpen(null)} />}
      <span className="sr-only">{t("chat.photos", { count: n })}</span>
    </div>
  );
}

/** Документы и стёртые вложения — карточками. */
export function Documents({ files, mine }: { files: ChatFile[]; mine: boolean }) {
  const { t } = useTranslation();
  const icon = `flex size-10 shrink-0 items-center justify-center rounded-full ${mine ? "bg-white/20 text-white" : "bg-accent text-paper"}`;
  return (
    <div className="space-y-1">
      {files.map((f) =>
        f.expired ? (
          <div key={f.id} className="flex items-center gap-2.5 py-1 opacity-70">
            <span className={icon}>
              <IconFileText size={18} />
            </span>
            <span className="min-w-0 flex-1">
              <span className="block truncate line-through">{f.name}</span>
              <span className="text-xs">{t("chat.expired")}</span>
            </span>
          </div>
        ) : (
          <a key={f.id} href={chatFileUrl(f.id)} download={f.name} className="group/doc flex items-center gap-2.5 rounded-lg py-1 !no-underline">
            <span className={`${icon} transition-transform group-hover/doc:scale-105`}>
              <IconFileText size={18} />
            </span>
            <span className="min-w-0 flex-1">
              <span className="block truncate font-medium">{f.name}</span>
              <span className={`text-xs tabular-nums ${mine ? "text-white/75" : "text-muted"}`}>
                {formatSize(f.size)} · {t("common.download")}
              </span>
            </span>
          </a>
        ),
      )}
    </div>
  );
}

/** Реакции под сообщением: нажатие ставит или снимает свою. */
export function Reactions({ items, onToggle, disabled, mine }: { items: ChatReaction[]; onToggle: (emoji: string) => void; disabled?: boolean; mine: boolean }) {
  const { t } = useTranslation();
  if (items.length === 0) return null;
  const tone = (on: boolean) =>
    mine ? (on ? "bg-white text-accent-ink" : "bg-white/20 text-white hover:bg-white/30") : on ? "bg-accent text-paper" : "bg-accentsoft text-accent-ink hover:bg-accent/20";
  return (
    <div className="mt-1.5 flex flex-wrap gap-1">
      {items.map((r) => (
        <button
          key={r.emoji}
          type="button"
          disabled={disabled}
          aria-pressed={r.mine}
          title={r.names.join(", ")}
          aria-label={t("chat.reactionLabel", { emoji: r.emoji, count: r.count, names: r.names.join(", ") })}
          onClick={() => onToggle(r.emoji)}
          className={`inline-flex h-7 items-center gap-1 rounded-full px-2 text-[13px] font-semibold tabular-nums transition-colors ${tone(r.mine)}`}
        >
          <span className="emoji text-base leading-none">{r.emoji}</span>
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
    if (focusFirst) box.current?.querySelector<HTMLElement>("button, input")?.focus();
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
    <div ref={box} role="group" aria-label={t("chat.react")} className="flex w-max max-w-[18rem] animate-pop-in flex-wrap gap-0.5 rounded-full border border-line bg-paper px-1.5 py-1 shadow-xl">
      {REACTIONS.map((e) => (
        <button key={e} type="button" className="emoji rounded-full p-1 text-[22px] leading-none transition-transform hover:scale-125 hover:bg-hover" aria-label={e} onClick={() => onPick(e)}>
          {e}
        </button>
      ))}
    </div>
  );
}

type PanelProps = PickerProps & { onGif?: (gif: string) => void; onKlipy?: (gif: ChatKlipyGif) => void; gifBusy?: boolean };

/** Панель поля ввода: эмодзи (поиск, недавние, разделы), «Мои GIF» и поиск KLIPY (если включён). */
export function EmojiPanel({ onPick, onClose, toggle, onGif, onKlipy, gifBusy }: PanelProps) {
  const { t } = useTranslation();
  const box = useRef<HTMLDivElement>(null);
  const [tab, setTab] = useState<"emoji" | "gif" | "klipy">("emoji");
  const klipy = useKlipy().data;
  // Поиск выключили, пока панель открыта, — вкладки KLIPY больше нет.
  const shown = tab === "klipy" && !(klipy && onKlipy) ? "emoji" : tab;
  usePopover(box, toggle, onClose, false);
  const tabClass = (on: boolean) => `flex-1 rounded-full py-1.5 text-sm font-medium transition-colors ${on ? "bg-accent text-paper" : "text-muted hover:bg-hover hover:text-ink"}`;
  return (
    <div ref={box} role="dialog" aria-label={t("chat.emoji")} className="absolute bottom-full left-0 z-20 mb-2 flex h-[clamp(14rem,calc(100dvh-16rem),25rem)] w-[22rem] max-w-[calc(100vw-1.5rem)] animate-pop-in flex-col overflow-hidden rounded-2xl border border-line bg-paper shadow-2xl">
      <div className="min-h-0 flex-1">
        {shown === "klipy" && klipy ? <KlipyTab cfg={klipy} onKlipy={onKlipy} busy={gifBusy} /> : shown === "emoji" ? <EmojiTab onPick={onPick} /> : <GifTab onGif={onGif} busy={gifBusy} />}
      </div>
      {onGif && (
        <div className="flex gap-1 border-t border-line p-1.5">
          <button type="button" aria-pressed={shown === "emoji"} className={tabClass(shown === "emoji")} onClick={() => setTab("emoji")}>
            {t("chat.emojiTab")}
          </button>
          <button type="button" aria-pressed={shown === "gif"} className={tabClass(shown === "gif")} onClick={() => setTab("gif")}>
            GIF
          </button>
          {klipy && onKlipy && (
            <button type="button" aria-pressed={shown === "klipy"} className={tabClass(shown === "klipy")} onClick={() => setTab("klipy")}>
              KLIPY
            </button>
          )}
        </div>
      )}
    </div>
  );
}

function EmojiTab({ onPick }: { onPick: (emoji: string) => void }) {
  const { t } = useTranslation();
  const [q, setQ] = useState("");
  const [recent, setRecent] = useState(recentEmoji);
  const scroller = useRef<HTMLDivElement>(null);
  const found = q.trim() ? searchEmoji(q) : null;
  const pick = (e: string) => {
    rememberEmoji(e);
    setRecent(recentEmoji());
    onPick(e);
  };
  const grid = (list: string[]) => (
    <div className="grid grid-cols-8 gap-0.5 px-2">
      {list.map((e) => (
        <button key={e} type="button" className="emoji flex aspect-square items-center justify-center overflow-hidden rounded-lg text-2xl leading-none transition-transform hover:scale-110 hover:bg-hover" aria-label={e} onClick={() => pick(e)}>
          {e}
        </button>
      ))}
    </div>
  );
  const heading = (text: string, id?: string) => (
    <h4 id={id} className="sticky top-0 z-[1] bg-paper/95 px-3 pt-2 pb-1 text-xs font-semibold text-muted backdrop-blur">
      {text}
    </h4>
  );
  const jump = (key: string) => {
    const el = document.getElementById(`emoji-${key}`);
    if (el && scroller.current) scroller.current.scrollTo({ top: el.offsetTop, behavior: "smooth" });
  };
  return (
    <div className="flex h-full flex-col">
      <div className="p-2 pb-1">
        <label className="flex items-center gap-2 rounded-full bg-sunken px-3 focus-within:ring-2 focus-within:ring-accent/40">
          <IconSearch size={15} className="text-muted" />
          <input type="search" value={q} onChange={(e) => setQ(e.target.value)} placeholder={t("chat.emojiSearch")} aria-label={t("chat.emojiSearch")} className="min-w-0 flex-1 bg-transparent py-1.5 text-sm outline-none focus-visible:outline-none" />
        </label>
      </div>
      {!found && (
        <div className="flex justify-between px-2 pb-1">
          {EMOJI_GROUPS.map((g) => (
            <button key={g.key} type="button" className="emoji rounded-lg p-1 text-lg leading-none opacity-70 transition hover:bg-hover hover:opacity-100" title={t(`chat.emojiGroups.${g.key}`)} aria-label={t(`chat.emojiGroups.${g.key}`)} onClick={() => jump(g.key)}>
              {g.icon}
            </button>
          ))}
        </div>
      )}
      <div ref={scroller} className="relative min-h-0 flex-1 overflow-y-auto pb-2">
        {found ? (
          found.length ? (
            grid(found)
          ) : (
            <p className="px-3 py-6 text-center text-sm text-muted">{t("chat.nothingFound")}</p>
          )
        ) : (
          <>
            {recent.length > 0 && (
              <section>
                {heading(t("chat.emojiRecent"))}
                {grid(recent)}
              </section>
            )}
            {EMOJI_GROUPS.map((g) => (
              <section key={g.key}>
                {heading(t(`chat.emojiGroups.${g.key}`), `emoji-${g.key}`)}
                {grid(g.items.map((x) => x.e))}
              </section>
            ))}
          </>
        )}
      </div>
    </div>
  );
}

/** «Мои GIF»: подборка сотрудника. Нажатие отправляет GIF в чат. */
function GifTab({ onGif, busy: sending }: { onGif?: (gif: string) => void; busy?: boolean }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const input = useRef<HTMLInputElement>(null);
  const [busy, setBusy] = useState(false);
  // Удаление — в два нажатия: первое показывает «Убрать?», второе убирает.
  const [armed, setArmed] = useState<string | null>(null);
  const list = useQuery({ queryKey: ["chat-gifs"], queryFn: () => ok<ChatGif[]>(api.GET("/api/chat/gifs")) });
  const upload = async (file: File) => {
    setBusy(true);
    try {
      await postFile<ChatGif>("/api/chat/gifs", file);
      void qc.invalidateQueries({ queryKey: ["chat-gifs"] });
    } catch (e) {
      toast((e instanceof Error && e.message) || t("common.error"), "bad");
    } finally {
      setBusy(false);
    }
  };
  const remove = async (id: string) => {
    try {
      await ok(api.DELETE("/api/chat/gifs/{gif}", { params: { path: { gif: id } } }));
      void qc.invalidateQueries({ queryKey: ["chat-gifs"] });
    } catch (e) {
      toast((e instanceof Error && e.message) || t("common.error"), "bad");
    }
  };
  const gifs = list.data ?? [];
  return (
    <div className="h-full overflow-y-auto p-2">
      <input
        ref={input}
        type="file"
        accept="image/gif"
        className="hidden"
        onChange={(e) => {
          const f = e.target.files?.[0];
          e.target.value = "";
          if (f) void upload(f);
        }}
      />
      <div className="grid grid-cols-3 gap-1">
        <button type="button" disabled={busy} className="flex aspect-square flex-col items-center justify-center gap-1 rounded-xl border-2 border-dashed border-line text-xs text-muted transition-colors hover:border-accent hover:text-accent disabled:opacity-50" onClick={() => input.current?.click()}>
          {busy ? <span className="size-5 animate-spin rounded-full border-2 border-accent border-t-transparent" aria-hidden="true" /> : <IconPlus size={20} />}
          {t("chat.gifAdd")}
        </button>
        {gifs.map((g, i) => (
          <div key={g.id} className="group/gif relative aspect-square overflow-hidden rounded-xl bg-sunken">
            <button type="button" disabled={sending} className="block size-full disabled:opacity-60" aria-label={`${t("chat.gifSend")} ${i + 1}`} onClick={() => onGif?.(g.id)}>
              <img src={`/api/chat/gifs/${g.id}`} alt="" loading="lazy" className="size-full object-cover transition-transform group-hover/gif:scale-105" />
            </button>
            {armed === g.id ? (
              <button type="button" className="absolute inset-x-1 bottom-1 rounded-lg bg-bad px-1 py-1.5 text-xs font-semibold text-white shadow" onClick={() => void remove(g.id)} onBlur={() => setArmed(null)}>
                {t("chat.gifRemove")}?
              </button>
            ) : (
              <button type="button" className="absolute top-0 right-0 flex size-8 items-center justify-center opacity-0 transition-opacity group-hover/gif:opacity-100 focus-visible:opacity-100 [@media(hover:none)]:opacity-100" aria-label={t("chat.gifRemove")} onClick={() => setArmed(g.id)}>
                <span className="rounded-full bg-black/55 p-1 text-white">
                  <IconX size={12} />
                </span>
              </button>
            )}
          </div>
        ))}
      </div>
      {list.isPending && <p className="px-2 pt-4 text-center text-sm text-muted">{t("common.loading")}</p>}
      {list.isError && <p className="px-2 pt-4 text-center text-sm text-bad">{t("common.error")}</p>}
      {list.isSuccess && gifs.length === 0 && <p className="px-2 pt-4 text-center text-sm text-muted">{t("chat.gifEmpty")}</p>}
    </div>
  );
}

/** Цитата: автор и начало сообщения (ответ, закреплённое, черновик ответа). */
export function Quote({ q, onClick, onClear, label, onColor = false }: { q: ChatQuote; onClick?: () => void; onClear?: () => void; label?: string; onColor?: boolean }) {
  const { t } = useTranslation();
  const text = q.preview || (q.files > 0 ? `📎 ${t("chat.files", { count: q.files })}` : q.gif ? "GIF" : "…");
  const inner = (
    <>
      <span className={`block truncate text-[13px] font-semibold ${onColor ? "text-white" : "text-accent"}`}>{label ?? q.author_name}</span>
      <span className={`block truncate text-[13px] ${onColor ? "text-white/80" : "text-muted"}`}>{label ? `${q.author_name}: ${text}` : text}</span>
    </>
  );
  return (
    <div className={`flex min-w-0 items-center gap-2 border-l-[3px] pl-2 ${onColor ? "border-white/80" : "border-accent"}`}>
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
