// Части экранов Helpdesk: текст заявки, редактор со вставкой скриншотов, плашки, объявления.
import { Link } from "@tanstack/react-router";
import { useEffect, useRef, useState, type ClipboardEvent, type DragEvent, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import Markdown from "react-markdown";
import { ApiError, type AnnouncementView, type AttachmentView } from "../../api/client";
import { Badge, Button } from "../../ui";
import { dyn, fileUrl, formatSize, priorityTone, statusTone, uploadFile, useAnnouncements, type Uploaded } from "./helpdeskUtil";

/** Картинка в тексте — только вложение Pult (точный адрес, без `..` и других путей). */
const FILE_URL = /^\/api\/helpdesk\/files\/[0-9a-f-]{36}$/i;

export function StatusBadge({ status }: { status: string }) {
  const { t } = useTranslation();
  return <Badge tone={statusTone[status] ?? "muted"}>{dyn(t, `helpdesk.status.${status}`)}</Badge>;
}

export function PriorityBadge({ priority }: { priority: string }) {
  const { t } = useTranslation();
  if (priority === "normal") return null;
  return <Badge tone={priorityTone[priority] ?? "muted"}>{dyn(t, `helpdesk.priority.${priority}`)}</Badge>;
}

/** Markdown без HTML; картинки — только вложения портала (внешние заблокировала бы и CSP). */
export function RichText({ text, keepBreaks = true }: { text: string; keepBreaks?: boolean }) {
  // keepBreaks: одиночный перенос строки в комментарии остаётся переносом. Ответу модели это не нужно:
  // переводы строк между блоками markdown дали бы лишние пустые строки.
  return (
    <div className={`rich-text space-y-2 text-sm break-words ${keepBreaks ? "whitespace-pre-wrap" : ""}`}>
      <Markdown
        components={{
          a: ({ href, children }) => (
            <a href={href} target="_blank" rel="noreferrer noopener" className="text-accent underline">
              {children}
            </a>
          ),
          img: ({ src, alt }) =>
            typeof src === "string" && FILE_URL.test(src) ? (
              <a href={src} target="_blank" rel="noreferrer">
                <img src={src} alt={alt ?? ""} className="max-h-80 rounded-lg border border-line" />
              </a>
            ) : (
              <span className="text-muted">{alt}</span>
            ),
        }}
      >
        {text}
      </Markdown>
    </div>
  );
}

export function AttachmentList({ items, text = "" }: { items: AttachmentView[]; text?: string }) {
  const shown = items.filter((a) => !text.includes(fileUrl(a.id)));
  if (shown.length === 0) return null;
  return (
    <ul className="mt-2 flex flex-wrap gap-2">
      {shown.map((a) => (
        <li key={a.id}>
          {a.is_image ? (
            <a href={fileUrl(a.id)} target="_blank" rel="noreferrer">
              <img src={fileUrl(a.id)} alt={a.name} className="h-24 rounded-lg border border-line object-cover" />
            </a>
          ) : (
            <a href={fileUrl(a.id)} className="inline-flex items-center gap-2 rounded-lg border border-line px-2 py-1 text-xs hover:bg-hover">
              {a.name} <span className="text-muted">{formatSize(a.size)}</span>
            </a>
          )}
        </li>
      ))}
    </ul>
  );
}

/** Текст со вставкой скриншота (Ctrl+V), перетаскиванием и фото с камеры. */
export function Editor({
  value,
  onChange,
  files,
  onFiles,
  placeholder,
  rows = 5,
  textRef,
  tone,
  disabled,
  onBusy,
  label,
}: {
  value: string;
  onChange: (v: string) => void;
  files: Uploaded[];
  onFiles: (f: Uploaded[]) => void;
  placeholder?: string;
  rows?: number;
  textRef?: React.RefObject<HTMLTextAreaElement | null>;
  tone?: "internal";
  disabled?: boolean;
  /** Идёт загрузка файлов: отправку на это время надо заблокировать, иначе вложение потеряется. */
  onBusy?: (busy: boolean) => void;
  /** Подпись поля для экранного диктора, если видимой подписи нет. */
  label?: string;
}) {
  const { t } = useTranslation();
  const [busy, setBusyState] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const input = useRef<HTMLInputElement>(null);
  // Актуальные текст и файлы: загрузка идёт долго, а пользователь тем временем печатает и вставляет ещё.
  const latest = useRef({ value, files });
  useEffect(() => {
    latest.current = { value, files };
  }, [value, files]);
  const setBusy = (b: boolean) => {
    setBusyState(b);
    onBusy?.(b);
  };
  const add = async (list: File[]) => {
    if (list.length === 0) return;
    setBusy(true);
    setError(null);
    for (const f of list) {
      try {
        const u = await uploadFile(f);
        const cur = latest.current;
        const text = u.is_image ? `${cur.value}${cur.value && !cur.value.endsWith("\n") ? "\n" : ""}![${u.name}](${fileUrl(u.id)})\n` : cur.value;
        latest.current = { value: text, files: [...cur.files, u] };
        if (text !== cur.value) onChange(text);
        onFiles(latest.current.files);
      } catch (e) {
        setError(e instanceof ApiError ? `${f.name}: ${e.message}` : t("common.error"));
      }
    }
    setBusy(false);
  };
  const paste = (e: ClipboardEvent<HTMLTextAreaElement>) => {
    const images = [...e.clipboardData.files].filter((f) => f.type.startsWith("image/"));
    if (images.length > 0) {
      e.preventDefault();
      void add(images);
    }
  };
  const drop = (e: DragEvent) => {
    e.preventDefault();
    void add([...e.dataTransfer.files]);
  };
  return (
    <div onDrop={drop} onDragOver={(e) => e.preventDefault()}>
      <textarea
        ref={textRef}
        aria-label={label ?? placeholder}
        rows={rows}
        value={value}
        disabled={disabled}
        placeholder={placeholder}
        onChange={(e) => onChange(e.target.value)}
        onPaste={paste}
        className={`w-full rounded-lg border px-3 py-2 text-sm focus:border-accent ${tone === "internal" ? "border-yellow-500/50 bg-yellow-500/10" : "border-control bg-paper"}`}
      />
      <div className="mt-1 flex flex-wrap items-center gap-2 text-xs text-muted">
        <Button type="button" variant="ghost" onClick={() => input.current?.click()} disabled={busy}>
          {t("helpdesk.attach")}
        </Button>
        <input
          ref={input}
          type="file"
          multiple
          className="hidden"
          onChange={(e) => {
            void add([...(e.target.files ?? [])]);
            e.target.value = "";
          }}
        />
        <span className="hidden md:inline">{busy ? t("helpdesk.uploading") : t("helpdesk.pasteHint")}</span>
        {files.map((f) => (
          <span key={f.id} className="inline-flex items-center gap-1 rounded bg-sunken px-1.5 py-0.5">
            {f.name}
            <button type="button" aria-label={t("common.delete")} onClick={() => onFiles(files.filter((x) => x.id !== f.id))}>
              ×
            </button>
          </span>
        ))}
      </div>
      {error && <div role="alert" className="mt-1 text-xs text-bad">{error}</div>}
    </div>
  );
}

/** Баннеры о массовых сбоях. */
export function Announcements({ action }: { action?: (a: AnnouncementView) => ReactNode }) {
  const q = useAnnouncements();
  if (!q.data?.length) return null;
  return (
    <div className="mb-4 space-y-2">
      {q.data.map((a) => (
        <div key={a.id} role="status" className="rounded-xl border border-bad/30 bg-bad/5 px-4 py-3 text-sm">
          <div className="font-semibold text-bad">{a.title}</div>
          {a.body && <div className="mt-1 whitespace-pre-wrap">{a.body}</div>}
          {action && <div className="mt-2">{action(a)}</div>}
        </div>
      ))}
    </div>
  );
}

export function TicketLink({ number, children, className = "", onClick }: { number: number; children: ReactNode; className?: string; onClick?: () => void }) {
  return (
    <Link to="/tickets/$number" params={{ number: String(number) }} className={className} onClick={onClick}>
      {children}
    </Link>
  );
}
