// Поиск GIF в KLIPY. По правилам KLIPY браузер сам ходит к их API и грузит анимации с их серверов
// (портал не проксирует и не хранит копий); в поле поиска — подсказка «Search KLIPY» (требование KLIPY).
import { useEffect, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { type ChatKlipyConfig, type ChatKlipyGif } from "../../api/client";
import { IconSearch } from "../../icons";

const API = "https://api.klipy.com/api/v1";
const PER_PAGE = 24;

type Variant = { url: string; width: number; height: number };
type Item = { slug: string; title?: string; type?: string; file?: Record<string, { gif?: Variant; webp?: Variant } | undefined> };
type Page = { items: Item[]; next: boolean };

/** Страна для подбора выдачи KLIPY по языку интерфейса. */
function locale(lang: string) {
  return lang.startsWith("zh") ? "cn" : lang.startsWith("en") ? "us" : "ru";
}

async function fetchPage(cfg: ChatKlipyConfig, q: string, page: number, lang: string): Promise<Page> {
  const kind = q ? "search" : "trending";
  const params = new URLSearchParams({
    page: String(page),
    per_page: String(PER_PAGE),
    customer_id: cfg.customer_id,
    locale: locale(lang),
    content_filter: "medium",
    format_filter: "gif,webp",
  });
  if (q) params.set("q", q);
  const r = await fetch(`${API}/${encodeURIComponent(cfg.app_key)}/gifs/${kind}?${params}`, { credentials: "omit", referrerPolicy: "no-referrer" });
  if (!r.ok) throw new Error(`KLIPY ${r.status}`);
  const body = (await r.json()) as { data?: { data?: Item[]; has_next?: boolean } };
  return { items: body.data?.data ?? [], next: !!body.data?.has_next };
}

/** Вариант анимации: сначала WebP (легче), потом GIF. */
function pick(item: Item, size: "sm" | "md" | "hd"): Variant | undefined {
  const f = item.file?.[size];
  return f?.webp ?? f?.gif;
}

/** Отметка «поделились» для KLIPY (их аналитика выдачи); ошибки не мешают отправке. */
function share(cfg: ChatKlipyConfig, slug: string, q: string) {
  void fetch(`${API}/${encodeURIComponent(cfg.app_key)}/gifs/share/${encodeURIComponent(slug)}`, {
    method: "POST",
    credentials: "omit",
    referrerPolicy: "no-referrer",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ customer_id: cfg.customer_id, q }),
  }).catch(() => undefined);
}

/** Вкладка поиска: популярное, пока запрос пуст; выдача — в порядке KLIPY, подгрузка при прокрутке. */
export function KlipyTab({ cfg, onKlipy, busy }: { cfg: ChatKlipyConfig; onKlipy?: (g: ChatKlipyGif) => void; busy?: boolean }) {
  const { t, i18n } = useTranslation();
  const [text, setText] = useState("");
  const [q, setQ] = useState("");
  const [pages, setPages] = useState<Page[]>([]);
  const [state, setState] = useState<"idle" | "loading" | "error">("loading");
  const end = useRef<HTMLDivElement>(null);
  const loading = useRef(false);
  const gen = useRef(0);

  // Запрос уходит, когда человек перестал печатать: у тестового ключа KLIPY 100 запросов в час.
  useEffect(() => {
    const id = window.setTimeout(() => setQ(text.trim()), 400);
    return () => window.clearTimeout(id);
  }, [text]);

  const load = (query: string, page: number, reset: boolean) => {
    if (loading.current && !reset) return;
    const mine = reset ? ++gen.current : gen.current;
    loading.current = true;
    setState("loading");
    fetchPage(cfg, query, page, i18n.language).then(
      (p) => {
        if (mine !== gen.current) return;
        setPages((old) => (reset ? [p] : [...old, p]));
        setState("idle");
        loading.current = false;
      },
      () => {
        if (mine !== gen.current) return;
        // Не удался новый поиск — прежняя выдача к нему не относится: «Повторить» начнёт с первой страницы.
        if (reset) setPages([]);
        setState("error");
        loading.current = false;
      },
    );
  };
  const loadRef = useRef(load);
  useEffect(() => {
    loadRef.current = load;
  });

  useEffect(() => {
    loadRef.current(q, 1, true);
  }, [q, cfg.app_key]);

  const more = pages.at(-1)?.next ?? false;
  useEffect(() => {
    const el = end.current;
    if (!el || !more) return;
    const io = new IntersectionObserver((e) => {
      if (e.some((x) => x.isIntersecting)) loadRef.current(q, pages.length + 1, false);
    });
    io.observe(el);
    return () => io.disconnect();
  }, [more, q, pages.length]);

  // Объявления KLIPY (type: "ad") портал не показывает: в Partner Panel они выключены по умолчанию.
  const items = pages.flatMap((p) => p.items).filter((x) => x.type !== "ad" && pick(x, "sm") && pick(x, "md"));
  const send = (x: Item) => {
    const v = pick(x, "md");
    if (!v || !onKlipy) return;
    onKlipy({ slug: x.slug, title: x.title ?? "", url: v.url, width: v.width, height: v.height });
    share(cfg, x.slug, q);
  };
  return (
    <div className="flex h-full flex-col">
      <label className="relative mx-2 mt-2 mb-1.5 block">
        <IconSearch size={15} className="pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-muted" />
        <input
          type="search"
          value={text}
          onChange={(e) => setText(e.target.value)}
          placeholder="Search KLIPY"
          aria-label="Search KLIPY"
          maxLength={100}
          className="w-full rounded-full bg-sunken py-1.5 pr-3 pl-8 text-sm outline-none focus-visible:ring-2 focus-visible:ring-accent/30"
        />
      </label>
      <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-2">
        <div className="grid grid-cols-2 gap-1">
          {items.map((x, i) => {
            const v = pick(x, "sm");
            return (
              <button key={`${x.slug}-${i}`} type="button" disabled={busy} className="group/gif relative aspect-square overflow-hidden rounded-xl bg-sunken disabled:opacity-60" aria-label={x.title || t("chat.gifSend")} title={x.title} onClick={() => send(x)}>
                <img src={v?.url} alt="" loading="lazy" referrerPolicy="no-referrer" className="size-full object-cover transition-transform group-hover/gif:scale-105" />
              </button>
            );
          })}
        </div>
        <div ref={end} className="h-6" />
        {state === "loading" && <p className="py-2 text-center text-sm text-muted">{t("common.loading")}</p>}
        {state === "error" && (
          <div className="py-3 text-center text-sm text-bad">
            {t("chat.klipyError")}{" "}
            <button type="button" className="font-semibold underline" onClick={() => load(q, pages.length + 1, pages.length === 0)}>
              {t("common.retry")}
            </button>
          </div>
        )}
        {state === "idle" && items.length === 0 && <p className="py-4 text-center text-sm text-muted">{t("chat.klipyEmpty")}</p>}
      </div>
      <div className="border-t border-line px-2 py-1 text-center text-[11px] text-muted">Powered by KLIPY</div>
    </div>
  );
}

/** GIF из KLIPY в переписке: анимация с серверов KLIPY; если её там убрали или поиск выключен — подпись. */
export function KlipyGif({ gif, overlay }: { gif: ChatKlipyGif; overlay?: ReactNode }) {
  const { t } = useTranslation();
  const [broken, setBroken] = useState(false);
  const ratio = Math.min(Math.max(gif.width / Math.max(gif.height, 1), 0.5), 2.5);
  return (
    <div className="relative w-full overflow-hidden bg-ink/10" style={{ aspectRatio: String(ratio) }}>
      {broken ? (
        <div className="flex size-full items-center justify-center p-3 text-center text-sm text-muted">
          {gif.title ? `${gif.title} — ` : ""}
          {t("chat.klipyGone")}
        </div>
      ) : (
        <img src={gif.url} alt={gif.title || "GIF"} loading="lazy" referrerPolicy="no-referrer" className="size-full object-cover" onError={() => setBroken(true)} />
      )}
      <span className="absolute top-1.5 left-1.5 rounded bg-black/50 px-1 text-[10px] font-bold tracking-wide text-white">GIF</span>
      {overlay && <div className="pointer-events-none absolute right-1.5 bottom-1.5">{overlay}</div>}
    </div>
  );
}
