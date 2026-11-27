// Статья базы знаний «как в Telegraph»: крупный заголовок, спокойный шрифт с засечками, картинки на всю
// ширину колонки с подписью, цитаты и код. Разметка — markdown; ссылки открываются в новой вкладке,
// картинки — только вложения портала (внешние адреса закрыты CSP и не подгружаются).
import { useMemo, useState, type ReactNode } from "react";
import Markdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import { useTranslation } from "react-i18next";
import type { KbFile } from "../../api/client";
import { IconFileText } from "../../icons";
import { Lightbox, type LightboxItem } from "../../lightbox";
import { fileUrl, formatSize } from "../helpdesk/helpdeskUtil";
import { headings, looksLikeFileName } from "./kbUtil";

const FILE_URL = /^\/api\/helpdesk\/files\/[0-9a-f-]{36}$/i;

/** Текст статьи. Заголовки получают якоря оглавления; картинки открываются во весь экран. */
export function TelegraphBody({ text }: { text: string }) {
  // Все картинки статьи по порядку — для листания в просмотре.
  const images = useMemo<LightboxItem[]>(() => {
    const out: LightboxItem[] = [];
    for (const m of text.matchAll(/!\[([^\]]*)\]\(([^)\s]+)\)/g)) {
      const src = m[2];
      if (src && FILE_URL.test(src)) out.push({ src, name: m[1] || "image" });
    }
    return out;
  }, [text]);
  const [open, setOpen] = useState<number | null>(null);
  // Разметка постоянна для одного текста: иначе при открытии картинки дерево статьи строилось бы заново.
  const components = useMemo<Components>(() => {
    // Якоря — те же, что в оглавлении (headings): по строке, на которой стоит заголовок.
    const ids = new Map(headings(text).map((h) => [h.line, h.id]));
    const heading = (Tag: "h2" | "h3", line: number | undefined, children: ReactNode) => (
      <Tag id={line ? ids.get(line) : undefined} className={Tag === "h2" ? "tg-h2" : "tg-h3"}>
        {children}
      </Tag>
    );
    return {
      h1: ({ children, node }) => heading("h2", node?.position?.start.line, children),
      h2: ({ children, node }) => heading("h2", node?.position?.start.line, children),
      h3: ({ children, node }) => heading("h3", node?.position?.start.line, children),
      h4: ({ children, node }) => heading("h3", node?.position?.start.line, children),
      h5: ({ children, node }) => heading("h3", node?.position?.start.line, children),
      h6: ({ children, node }) => heading("h3", node?.position?.start.line, children),
      a: ({ href, children }) =>
        typeof href === "string" && FILE_URL.test(href) ? (
          <a href={href} download className="tg-link">
            {children}
          </a>
        ) : (
          <a href={href} target="_blank" rel="noreferrer noopener" className="tg-link">
            {children}
          </a>
        ),
      // Абзац из одной картинки становится иллюстрацией с подписью.
      p: ({ children, node }) => {
        const first = node?.children[0];
        const only = node?.children.length === 1 && first?.type === "element" && first.tagName === "img";
        return only ? <>{children}</> : <p>{children}</p>;
      },
      // Широкая таблица прокручивается вбок, а не ломает страницу.
      table: ({ children }) => (
        <div className="tg-table" tabIndex={0}>
          <table>{children}</table>
        </div>
      ),
      img: ({ src, alt }) => {
        if (typeof src !== "string" || !FILE_URL.test(src)) return <span className="text-muted">{alt}</span>;
        const i = images.findIndex((x) => x.src === src);
        const caption = alt && !looksLikeFileName(alt) ? alt : null;
        return (
          <figure className="tg-figure">
            <button type="button" className="block w-full cursor-zoom-in" onClick={() => setOpen(i < 0 ? 0 : i)}>
              <img src={src} alt={alt ?? ""} loading="lazy" />
            </button>
            {caption && <figcaption>{caption}</figcaption>}
          </figure>
        );
      },
    };
  }, [text, images]);
  return (
    <div className="telegraph-body">
      <Markdown components={components} remarkPlugins={PLUGINS}>
        {text}
      </Markdown>
      {open !== null && images.length > 0 && <Lightbox items={images} index={open} onIndex={setOpen} onClose={() => setOpen(null)} />}
    </div>
  );
}

/** Таблицы, ~~зачёркивание~~, списки задач и ссылки без скобок (GitHub Flavored Markdown). */
const PLUGINS = [remarkGfm];

/** Вложения статьи, которых нет в тексте: карточки для скачивания. */
export function KbFiles({ files, text }: { files: KbFile[]; text: string }) {
  const { t } = useTranslation();
  const shown = files.filter((f) => !text.includes(f.id));
  if (shown.length === 0) return null;
  return (
    <section className="mt-10 border-t border-line pt-6" aria-label={t("kb.files")}>
      <h2 className="mb-3 text-sm font-semibold text-muted">{t("kb.files")}</h2>
      <ul className="grid gap-2 sm:grid-cols-2">
        {shown.map((f) => (
          <li key={f.id}>
            <a href={fileUrl(f.id)} download={f.name} className="flex items-center gap-3 rounded-xl border border-line bg-paper px-3 py-2.5 hover:border-accent/50 hover:bg-hover">
              {f.is_image ? (
                <img src={fileUrl(f.id)} alt="" className="size-10 shrink-0 rounded-lg object-cover" loading="lazy" />
              ) : (
                <span className="flex size-10 shrink-0 items-center justify-center rounded-lg bg-accentsoft text-accent-ink">
                  <IconFileText size={20} />
                </span>
              )}
              <span className="min-w-0 flex-1">
                <span className="block truncate text-sm font-medium">{f.name}</span>
                <span className="text-xs text-muted tabular-nums">{formatSize(f.size)}</span>
              </span>
            </a>
          </li>
        ))}
      </ul>
    </section>
  );
}
