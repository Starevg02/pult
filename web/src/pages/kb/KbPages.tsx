// База знаний: поиск и разделы, статья «как в Telegraph», редактор с панелью разметки, вложениями и
// предпросмотром (в том числе черновик из решённой заявки).
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useBlocker, useNavigate, useParams, useSearch } from "@tanstack/react-router";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type Article, type ArticleRow, type KbFile, type KbSection } from "../../api/client";
import { formatDay } from "../../format";
import { IconArrowLeft, IconBook, IconFileText, IconLink, IconPaperclip, IconSearch, IconTrash, IconX } from "../../icons";
import { can, useMe } from "../../session";
import { Badge, Button, CopyButton, Empty, ErrorBox, Input, Select, Spinner } from "../../ui";
import { fileUrl, formatSize, uploadFile } from "../helpdesk/helpdeskUtil";
import { headings, insertBlock, link, prefixLines, readingMinutes, wrap, type Edit } from "./kbUtil";
import { KbFiles, TelegraphBody } from "./Telegraph";

function useSections() {
  return useQuery({ queryKey: ["kb-sections"], queryFn: () => ok<KbSection[]>(api.GET("/api/kb/sections")) });
}

const chip = (on: boolean) =>
  `shrink-0 rounded-full border px-3 py-1 text-sm transition-colors ${on ? "border-ink bg-ink text-paper" : "border-line bg-paper text-muted hover:border-line-strong hover:text-ink"}`;

export function KbPage() {
  const { t, i18n } = useTranslation();
  const me = useMe().data;
  const editor = can(me, "helpdesk.kb.edit");
  const [q, setQ] = useState("");
  const [needle, setNeedle] = useState("");
  const [section, setSection] = useState("");
  useEffect(() => {
    const h = setTimeout(() => setNeedle(q.trim()), 250);
    return () => clearTimeout(h);
  }, [q]);
  const sections = useSections();
  const list = useQuery({
    queryKey: ["kb", needle, section, editor],
    queryFn: () => ok<ArticleRow[]>(api.GET("/api/kb/articles", { params: { query: { q: needle || undefined, section_id: section || undefined, drafts: editor || undefined } } })),
  });
  return (
    <div className="mx-auto max-w-4xl">
      <header className="mb-6 flex flex-wrap items-end gap-3">
        <div className="min-w-0 flex-1">
          <h1 className="telegraph-title">{t("kb.title")}</h1>
          <p className="mt-1 text-sm text-muted">{t("kb.subtitle")}</p>
        </div>
        {editor && (
          <Link to="/kb/new" search={{ ticket: undefined }} className="rounded-lg bg-ink px-3.5 py-2 text-sm font-medium text-paper hover:opacity-90">
            {t("kb.newArticle")}
          </Link>
        )}
      </header>
      <div className="relative mb-4">
        <IconSearch size={18} className="pointer-events-none absolute top-1/2 left-3.5 -translate-y-1/2 text-muted" />
        <input
          type="search"
          aria-label={t("kb.search")}
          placeholder={t("kb.search")}
          value={q}
          onChange={(e) => setQ(e.target.value)}
          className="w-full rounded-xl border border-control bg-paper py-3 pr-4 pl-11 text-base shadow-sm focus:border-accent"
        />
      </div>
      {(sections.data?.length ?? 0) > 0 && (
        <div className="mb-5 flex gap-2 overflow-x-auto pb-1" role="group" aria-label={t("kb.section")}>
          <button type="button" className={chip(section === "")} aria-pressed={section === ""} onClick={() => setSection("")}>
            {t("kb.allSections")}
          </button>
          {sections.data?.map((s) => (
            <button key={s.id} type="button" className={chip(section === s.id)} aria-pressed={section === s.id} onClick={() => setSection(s.id)}>
              {s.name} <span className="ml-1 text-xs opacity-70 tabular-nums">{s.articles}</span>
            </button>
          ))}
        </div>
      )}
      {list.error ? (
        <ErrorBox error={list.error} onRetry={() => void list.refetch()} />
      ) : list.isPending ? (
        <Spinner variant="list" />
      ) : list.data.length === 0 ? (
        <Empty hint={q.trim() ? undefined : editor ? t("kb.emptyEditorHint") : undefined}>{q.trim() ? t("kb.notFound") : t("kb.empty")}</Empty>
      ) : (
        <ul className="divide-y divide-line overflow-hidden rounded-2xl border border-line bg-paper">
          {list.data.map((a) => (
            <li key={a.id}>
              <Link to="/kb/$id" params={{ id: a.id }} className="flex gap-4 px-4 py-4 hover:bg-hover sm:px-5">
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-center gap-1.5">
                    {a.status === "draft" && <Badge>{t("kb.draft")}</Badge>}
                    {a.audience === "engineers" && <Badge tone="accent">{t("kb.forEngineers")}</Badge>}
                  </div>
                  <h2 className="text-lg leading-snug font-semibold [font-family:Georgia,Cambria,serif]">{a.title}</h2>
                  {a.excerpt && <p className="mt-1 line-clamp-2 text-sm text-muted">{a.excerpt}</p>}
                  <div className="mt-2 flex flex-wrap gap-x-2 text-xs text-muted">
                    {a.section_name && <span>{a.section_name}</span>}
                    {a.section_name && <span aria-hidden="true">·</span>}
                    <span>{formatDay(a.updated_at, i18n.language)}</span>
                    <span aria-hidden="true">·</span>
                    <span>{t("kb.views", { n: a.views })}</span>
                  </div>
                </div>
                {a.cover ? (
                  <img src={fileUrl(a.cover)} alt="" loading="lazy" className="size-20 shrink-0 rounded-xl object-cover sm:h-24 sm:w-32" />
                ) : (
                  <span aria-hidden="true" className="hidden size-20 shrink-0 items-center justify-center rounded-xl bg-sunken text-muted sm:flex sm:h-24 sm:w-32">
                    <IconBook size={26} />
                  </span>
                )}
              </Link>
            </li>
          ))}
        </ul>
      )}
      {editor && <SectionEditor sections={sections.data ?? []} />}
    </div>
  );
}

function SectionEditor({ sections }: { sections: KbSection[] }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [name, setName] = useState("");
  const add = useMutation({
    mutationFn: () => ok(api.POST("/api/kb/sections", { body: { name, sort: sections.length * 10 } })),
    onSuccess: () => {
      setName("");
      void qc.invalidateQueries({ queryKey: ["kb-sections"] });
    },
  });
  const remove = useMutation({
    mutationFn: (id: string) => ok(api.DELETE("/api/kb/sections/{id}", { params: { path: { id } } })),
    onSuccess: () => void qc.invalidateQueries({ queryKey: ["kb-sections"] }),
  });
  const empty = sections.filter((s) => s.articles === 0);
  return (
    <details className="mt-6 rounded-xl border border-line bg-paper px-4 py-3 text-sm">
      <summary className="cursor-pointer font-medium">{t("kb.manageSections")}</summary>
      <form
        className="mt-3 flex gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          if (name.trim()) add.mutate();
        }}
      >
        <Input value={name} onChange={(e) => setName(e.target.value)} placeholder={t("kb.newSection")} aria-label={t("kb.newSection")} />
        <Button type="submit" disabled={!name.trim() || add.isPending}>
          {t("common.add")}
        </Button>
      </form>
      {empty.length > 0 && (
        <div className="mt-3 flex flex-wrap gap-1.5">
          {empty.map((s) => (
            <button key={s.id} type="button" className="inline-flex items-center gap-1 rounded-full bg-sunken px-2.5 py-0.5 text-xs text-muted hover:text-bad" onClick={() => remove.mutate(s.id)} aria-label={`${t("common.delete")}: ${s.name}`}>
              {s.name} <IconX size={12} />
            </button>
          ))}
        </div>
      )}
      {(add.error || remove.error) && <ErrorBox error={add.error ?? remove.error} />}
    </details>
  );
}

export function KbArticlePage() {
  const { t, i18n } = useTranslation();
  const { id } = useParams({ from: "/kb/$id" });
  const a = useQuery({ queryKey: ["kb-article", id], queryFn: () => ok<Article>(api.GET("/api/kb/articles/{id}", { params: { path: { id } } })) });
  const toc = useMemo(() => (a.data ? headings(a.data.body) : []), [a.data]);
  if (a.error) return <ErrorBox error={a.error} />;
  if (!a.data) return <Spinner />;
  const x = a.data;
  const minutes = readingMinutes(x.body);
  return (
    <div className="mx-auto max-w-3xl">
      <div className="mb-3 flex flex-wrap items-center gap-2 text-sm print:hidden">
        <Link to="/kb" className="inline-flex items-center gap-1 rounded-lg px-1.5 py-1 text-muted hover:bg-hover hover:text-ink">
          <IconArrowLeft size={16} /> {t("kb.title")}
        </Link>
        <span className="ml-auto flex items-center gap-1">
          <CopyButton value={window.location.href} label={t("kb.copyLink")} />
          <Button variant="ghost" onClick={() => window.print()}>
            {t("kb.print")}
          </Button>
          {x.can_edit && (
            <Link to="/kb/$id/edit" params={{ id: x.id }} className="rounded-lg border border-line-strong bg-paper px-3 py-1.5 font-medium hover:bg-hover">
              {t("common.edit")}
            </Link>
          )}
        </span>
      </div>
      <article className="rounded-2xl bg-paper px-5 py-8 shadow-sm ring-1 ring-line sm:px-12 sm:py-12 print:shadow-none print:ring-0">
        {(x.section_name || x.status === "draft" || x.audience === "engineers") && (
          <div className="mb-3 flex flex-wrap gap-1.5">
            {x.section_name && <Badge>{x.section_name}</Badge>}
            {x.status === "draft" && <Badge tone="bad">{t("kb.draft")}</Badge>}
            {x.audience === "engineers" && <Badge tone="accent">{t("kb.forEngineers")}</Badge>}
          </div>
        )}
        <h1 className="telegraph-title">{x.title}</h1>
        <p className="mt-3 flex flex-wrap gap-x-2 text-sm text-muted">
          {x.author_name && <span className="font-medium text-ink">{x.author_name}</span>}
          {x.author_name && <span aria-hidden="true">·</span>}
          <time dateTime={x.created_at}>{formatDay(x.created_at, i18n.language)}</time>
          <span aria-hidden="true">·</span>
          <span>{t("kb.minutes", { count: minutes })}</span>
          <span aria-hidden="true">·</span>
          <span>{t("kb.views", { n: x.views })}</span>
        </p>
        {toc.length >= 3 && (
          <nav aria-label={t("kb.contents")} className="mt-6 rounded-xl bg-sunken px-4 py-3 text-sm print:hidden">
            <div className="mb-1 font-semibold">{t("kb.contents")}</div>
            <ol className="space-y-0.5">
              {toc.map((h) => (
                <li key={h.id} className={h.level === 3 ? "pl-4" : ""}>
                  <a href={`#${h.id}`} className="text-accent hover:underline">
                    {h.text}
                  </a>
                </li>
              ))}
            </ol>
          </nav>
        )}
        <div className="mt-8">
          <TelegraphBody text={x.body} />
        </div>
        <KbFiles files={x.files} text={x.body} />
        <footer className="mt-10 flex flex-wrap gap-x-3 border-t border-line pt-4 text-xs text-muted">
          {x.updated_at !== x.created_at && <span>{t("kb.updated", { date: formatDay(x.updated_at, i18n.language) })}</span>}
          {x.source_ticket_number && x.can_edit && (
            <Link to="/tickets/$number" params={{ number: String(x.source_ticket_number) }} className="text-accent hover:underline">
              {t("kb.fromTicket", { n: x.source_ticket_number })}
            </Link>
          )}
        </footer>
      </article>
    </div>
  );
}

type FormState = { title: string; body: string; section_id: string; audience: string; status: string; source_ticket_number: number | null };

const fileRef = (id: string) => new RegExp(`!?\\[[^\\]]*\\]\\(${fileUrl(id).replace(/[/]/g, "\\/")}\\)\\n?`, "g");

type Tool = "b" | "i" | "h2" | "h3" | "q" | "ul" | "ol" | "code" | "hr";

function ArticleForm({ initial, initialFiles, id }: { initial: FormState; initialFiles: KbFile[]; id?: string }) {
  const { t } = useTranslation();
  const nav = useNavigate();
  const qc = useQueryClient();
  const sections = useSections();
  const [f, setF] = useState(initial);
  const [files, setFiles] = useState<KbFile[]>(initialFiles);
  const [mode, setMode] = useState<"write" | "preview" | "split">("write");
  const [uploading, setUploading] = useState(0);
  const [uploadError, setUploadError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const area = useRef<HTMLTextAreaElement>(null);
  const picker = useRef<HTMLInputElement>(null);
  const dirty = !saved && (JSON.stringify(f) !== JSON.stringify(initial) || files.length !== initialFiles.length);
  useEffect(() => {
    if (!dirty) return;
    const warn = (e: BeforeUnloadEvent) => e.preventDefault();
    window.addEventListener("beforeunload", warn);
    return () => window.removeEventListener("beforeunload", warn);
  }, [dirty]);
  // Переход внутри портала (назад, меню): после сохранения или удаления — без вопроса.
  const leaving = useRef(false);
  useBlocker({ shouldBlockFn: () => dirty && !leaving.current && !window.confirm(t("kb.leaveConfirm")), enableBeforeUnload: false });
  const body = { ...f, section_id: f.section_id || null, files: files.map((x) => x.id) };
  const save = useMutation({
    mutationFn: async (status: string) => {
      const b = { ...body, status };
      if (id) {
        await ok(api.PUT("/api/kb/articles/{id}", { params: { path: { id } }, body: b }));
        return id;
      }
      const r = (await ok(api.POST("/api/kb/articles", { body: b }))) as { id: string };
      return r.id;
    },
    onSuccess: (to) => {
      leaving.current = true;
      setSaved(true);
      void qc.invalidateQueries({ queryKey: ["kb"] });
      // Счётчики статей в разделах.
      void qc.invalidateQueries({ queryKey: ["kb-sections"] });
      void qc.invalidateQueries({ queryKey: ["kb-article", to] });
      void nav({ to: "/kb/$id", params: { id: to } });
    },
  });
  const remove = useMutation({
    mutationFn: () => ok(api.DELETE("/api/kb/articles/{id}", { params: { path: { id: id ?? "" } } })),
    onSuccess: () => {
      leaving.current = true;
      setSaved(true);
      void qc.invalidateQueries({ queryKey: ["kb"] });
      void qc.invalidateQueries({ queryKey: ["kb-sections"] });
      void nav({ to: "/kb" });
    },
  });
  const ready = f.title.trim() !== "" && f.body.trim() !== "" && uploading === 0 && !save.isPending;

  /** Правка текста кнопкой панели: от текущего текста (и после загрузки файла — тоже), выделение
   *  после правки остаётся на изменённом. */
  const selection = useRef<Edit | null>(null);
  const apply = (fn: (e: Edit) => Edit) => {
    const el = area.current;
    setF((s) => {
      const next = fn({ value: s.body, start: el?.selectionStart ?? s.body.length, end: el?.selectionEnd ?? s.body.length });
      selection.current = next;
      return { ...s, body: next.value };
    });
    requestAnimationFrame(() => {
      const next = selection.current;
      if (!el || !next) return;
      el.focus();
      el.setSelectionRange(next.start, next.end);
    });
  };
  const upload = async (list: File[]) => {
    if (list.length === 0) return;
    setUploadError(null);
    setUploading((n) => n + list.length);
    for (const file of list) {
      try {
        const u = await uploadFile(file);
        const kb: KbFile = { id: u.id, name: u.name, is_image: u.is_image, size: file.size, mime: file.type || "application/octet-stream" };
        setFiles((xs) => [...xs, kb]);
        if (u.is_image) apply((e) => insertBlock(e, `![${u.name}](${fileUrl(u.id)})`));
      } catch (e) {
        setUploadError(`${file.name}: ${e instanceof Error ? e.message : t("common.error")}`);
      } finally {
        setUploading((n) => n - 1);
      }
    }
  };
  // Кнопки панели: описание отдельно от действия (действие читает поле ввода только по нажатию).
  const tools: { key: Tool; label: string; text: string; className?: string }[] = [
    { key: "b", label: t("kb.tools.bold"), text: "B", className: "font-bold" },
    { key: "i", label: t("kb.tools.italic"), text: "I", className: "italic font-serif" },
    { key: "h2", label: t("kb.tools.h2"), text: "H2" },
    { key: "h3", label: t("kb.tools.h3"), text: "H3" },
    { key: "q", label: t("kb.tools.quote"), text: "❝" },
    { key: "ul", label: t("kb.tools.list"), text: "•" },
    { key: "ol", label: t("kb.tools.numbered"), text: "1." },
    { key: "code", label: t("kb.tools.code"), text: "</>", className: "font-mono text-xs" },
    { key: "hr", label: t("kb.tools.divider"), text: "—" },
  ];
  const runTool = (k: Tool) => {
    const sample = t("kb.tools.sample");
    const edits: Record<Tool, (e: Edit) => Edit> = {
      b: (e) => wrap(e, "**", "**", sample),
      i: (e) => wrap(e, "_", "_", sample),
      h2: (e) => prefixLines(e, "## "),
      h3: (e) => prefixLines(e, "### "),
      q: (e) => prefixLines(e, "> "),
      ul: (e) => prefixLines(e, "- "),
      ol: (e) => prefixLines(e, (n) => `${n + 1}. `),
      code: (e) => (e.value.slice(e.start, e.end).includes("\n") ? wrap(e, "```\n", "\n```", "") : wrap(e, "`", "`", "code")),
      hr: (e) => insertBlock(e, "---"),
    };
    apply(edits[k]);
  };
  const addLink = () => {
    const url = window.prompt(t("kb.tools.linkPrompt"), "https://");
    if (url && url !== "https://") apply((e) => link(e, url.trim(), t("kb.tools.linkText")));
  };
  const showEditor = mode !== "preview";
  const showPreview = mode !== "write";
  return (
    <div className="mx-auto max-w-6xl">
      <div className="sticky top-0 z-10 -mx-2 mb-4 flex flex-wrap items-center gap-2 border-b border-line bg-canvas/95 px-2 py-2 backdrop-blur">
        {id ? (
          <Link to="/kb/$id" params={{ id }} className="inline-flex items-center gap-1 rounded-lg px-1.5 py-1 text-sm text-muted hover:bg-hover hover:text-ink">
            <IconArrowLeft size={16} /> {t("kb.backToArticle")}
          </Link>
        ) : (
          <Link to="/kb" className="inline-flex items-center gap-1 rounded-lg px-1.5 py-1 text-sm text-muted hover:bg-hover hover:text-ink">
            <IconArrowLeft size={16} /> {t("kb.title")}
          </Link>
        )}
        <span className="text-sm text-muted">{dirty ? t("kb.unsaved") : ""}</span>
        <div className="ml-auto flex flex-wrap items-center gap-2">
          <div className="inline-flex rounded-lg border border-line bg-paper p-0.5 text-sm" role="group" aria-label={t("kb.view")}>
            {(["write", "split", "preview"] as const).map((m) => (
              <button key={m} type="button" aria-pressed={mode === m} className={`rounded-md px-2.5 py-1 ${m === "split" ? "hidden lg:block" : ""} ${mode === m ? "bg-ink text-paper" : "text-muted hover:text-ink"}`} onClick={() => setMode(m)}>
                {t(`kb.modes.${m}`)}
              </button>
            ))}
          </div>
          <Button disabled={!ready} onClick={() => save.mutate("draft")}>
            {t("kb.saveDraft")}
          </Button>
          <Button variant="primary" disabled={!ready} onClick={() => save.mutate("published")}>
            {f.status === "published" ? t("kb.saveAndPublish") : t("kb.publish")}
          </Button>
        </div>
      </div>
      {(save.error || remove.error) && <ErrorBox error={save.error ?? remove.error} />}
      <div className={`grid gap-6 ${mode === "split" ? "lg:grid-cols-2" : ""}`}>
        {showEditor && (
          <div className="min-w-0">
            <textarea
              aria-label={t("kb.articleTitle")}
              placeholder={t("kb.articleTitle")}
              value={f.title}
              maxLength={300}
              rows={1}
              onChange={(e) => setF({ ...f, title: e.target.value.replace(/\n/g, " ") })}
              className="telegraph-title field-sizing-content w-full resize-none bg-transparent outline-none placeholder:text-muted/60"
            />
            <div className="mt-3 flex flex-wrap gap-2 text-sm">
              <Select aria-label={t("kb.section")} value={f.section_id} onChange={(e) => setF({ ...f, section_id: e.target.value })} className="w-auto">
                <option value="">{t("kb.noSection")}</option>
                {sections.data?.map((s) => (
                  <option key={s.id} value={s.id}>
                    {s.name}
                  </option>
                ))}
              </Select>
              <Select aria-label={t("kb.audience")} value={f.audience} onChange={(e) => setF({ ...f, audience: e.target.value })} className="w-auto">
                <option value="all">{t("kb.audienceAll")}</option>
                <option value="engineers">{t("kb.forEngineers")}</option>
              </Select>
              {f.status === "published" && <Badge tone="ok">{t("kb.published")}</Badge>}
            </div>
            <div className="sticky top-14 z-[5] mt-4 flex flex-wrap items-center gap-0.5 rounded-xl border border-line bg-paper p-1 shadow-sm" role="toolbar" aria-label={t("kb.tools.title")}>
              {tools.map((x) => (
                <button key={x.key} type="button" title={x.label} aria-label={x.label} onClick={() => runTool(x.key)} className={`min-w-8 rounded-lg px-2 py-1 text-sm hover:bg-hover ${x.className ?? ""}`}>
                  {x.text}
                </button>
              ))}
              <button type="button" title={t("kb.tools.link")} aria-label={t("kb.tools.link")} onClick={addLink} className="rounded-lg px-2 py-1.5 hover:bg-hover">
                <IconLink size={16} />
              </button>
              <span className="mx-1 h-5 w-px bg-line" aria-hidden="true" />
              <button type="button" onClick={() => picker.current?.click()} className="inline-flex items-center gap-1.5 rounded-lg px-2 py-1 text-sm hover:bg-hover">
                <IconPaperclip size={16} /> {t("kb.tools.attach")}
              </button>
              <input
                ref={picker}
                type="file"
                multiple
                className="hidden"
                onChange={(e) => {
                  void upload([...(e.target.files ?? [])]);
                  e.target.value = "";
                }}
              />
              {uploading > 0 && (
                <span role="status" className="ml-2 text-xs text-muted">
                  {t("kb.uploading")}
                </span>
              )}
            </div>
            <textarea
              ref={area}
              aria-label={t("kb.body")}
              placeholder={t("kb.bodyPlaceholder")}
              value={f.body}
              onChange={(e) => setF({ ...f, body: e.target.value })}
              onPaste={(e) => {
                const pasted = [...e.clipboardData.files];
                if (pasted.length > 0 && !e.clipboardData.getData("text/plain")) {
                  e.preventDefault();
                  void upload(pasted);
                }
              }}
              onDragOver={(e) => {
                if ([...e.dataTransfer.types].includes("Files")) e.preventDefault();
              }}
              onDrop={(e) => {
                if (e.dataTransfer.files.length === 0) return;
                e.preventDefault();
                void upload([...e.dataTransfer.files]);
              }}
              onKeyDown={(e) => {
                const mod = e.ctrlKey || e.metaKey;
                if (!mod) return;
                const k = e.key.toLowerCase();
                if (k === "b" || k === "и") {
                  e.preventDefault();
                  apply((x) => wrap(x, "**", "**", t("kb.tools.sample")));
                } else if (k === "i" || k === "ш") {
                  e.preventDefault();
                  apply((x) => wrap(x, "_", "_", t("kb.tools.sample")));
                } else if (k === "k" || k === "л") {
                  e.preventDefault();
                  addLink();
                } else if (k === "s" || k === "ы") {
                  e.preventDefault();
                  if (ready) save.mutate(f.status);
                }
              }}
              className="mt-3 field-sizing-content min-h-[50vh] w-full resize-y rounded-xl border border-line bg-paper px-4 py-3 text-[15px] leading-relaxed focus:border-accent"
            />
            <p className="mt-1 text-xs text-muted">{t("kb.markdownHint")}</p>
            {uploadError && (
              <p role="alert" className="mt-1 text-sm text-bad">
                {uploadError}
              </p>
            )}
            {files.length > 0 && (
              <ul className="mt-4 space-y-1.5" aria-label={t("kb.files")}>
                {files.map((x) => {
                  const inText = f.body.includes(x.id);
                  return (
                    <li key={x.id} className="flex items-center gap-2 rounded-lg border border-line bg-paper px-2.5 py-1.5 text-sm">
                      {x.is_image ? <img src={fileUrl(x.id)} alt="" className="size-8 rounded object-cover" /> : <IconFileText size={18} className="text-muted" />}
                      <span className="min-w-0 flex-1 truncate">{x.name}</span>
                      <span className="text-xs text-muted tabular-nums">{formatSize(x.size)}</span>
                      {inText ? (
                        <Badge>{t("kb.inText")}</Badge>
                      ) : (
                        <button type="button" className="text-xs text-accent hover:underline" onClick={() => apply((e) => (x.is_image ? insertBlock(e, `![${x.name}](${fileUrl(x.id)})`) : link(e, fileUrl(x.id), x.name)))}>
                          {t("kb.insert")}
                        </button>
                      )}
                      <button
                        type="button"
                        className="rounded p-1 text-muted hover:bg-hover hover:text-bad"
                        aria-label={t("chat.removeFile", { name: x.name })}
                        onClick={() => {
                          setFiles((xs) => xs.filter((y) => y.id !== x.id));
                          setF((s) => ({ ...s, body: s.body.replace(fileRef(x.id), "") }));
                        }}
                      >
                        <IconTrash size={14} />
                      </button>
                    </li>
                  );
                })}
              </ul>
            )}
            {id && (
              <div className="mt-8 border-t border-line pt-4">
                <Button variant="danger" onClick={() => window.confirm(t("kb.deleteConfirm")) && remove.mutate()}>
                  {t("kb.deleteArticle")}
                </Button>
              </div>
            )}
          </div>
        )}
        {showPreview && (
          <article className="min-w-0 rounded-2xl bg-paper px-5 py-8 shadow-sm ring-1 ring-line sm:px-10" aria-label={t("kb.modes.preview")}>
            <h1 className="telegraph-title">{f.title || t("kb.articleTitle")}</h1>
            <div className="mt-6">{f.body.trim() ? <TelegraphBody text={f.body} /> : <p className="text-muted">{t("kb.bodyPlaceholder")}</p>}</div>
            <KbFiles files={files} text={f.body} />
          </article>
        )}
      </div>
    </div>
  );
}

export function KbNewPage() {
  const { ticket } = useSearch({ from: "/kb/new" });
  const draft = useQuery({
    queryKey: ["kb-draft", ticket],
    queryFn: () => ok<{ title: string; body: string; source_ticket_number: number; files: KbFile[] }>(api.GET("/api/helpdesk/tickets/{number}/kb-draft", { params: { path: { number: ticket ?? 0 } } })),
    enabled: !!ticket,
    // Каждый запрос черновика копирует вложения заявки — не повторять его зря.
    staleTime: Infinity,
    gcTime: 0,
  });
  if (ticket && draft.error) return <ErrorBox error={draft.error} />;
  if (ticket && !draft.data) return <Spinner />;
  return (
    <ArticleForm
      initial={{ title: draft.data?.title ?? "", body: draft.data?.body ?? "", section_id: "", audience: "all", status: "draft", source_ticket_number: draft.data?.source_ticket_number ?? null }}
      initialFiles={draft.data?.files ?? []}
    />
  );
}

export function KbEditPage() {
  const { id } = useParams({ from: "/kb/$id/edit" });
  const a = useQuery({ queryKey: ["kb-article", id], queryFn: () => ok<Article>(api.GET("/api/kb/articles/{id}", { params: { path: { id } } })) });
  if (a.error) return <ErrorBox error={a.error} />;
  if (!a.data) return <Spinner />;
  const x = a.data;
  return (
    <ArticleForm
      id={id}
      initial={{ title: x.title, body: x.body, section_id: x.section_id ?? "", audience: x.audience, status: x.status, source_ticket_number: x.source_ticket_number ?? null }}
      initialFiles={x.files}
    />
  );
}

/** Подсказки из базы знаний по теме новой заявки. */
export function KbSuggest({ text }: { text: string }) {
  const { t } = useTranslation();
  const [needle, setNeedle] = useState("");
  useEffect(() => {
    const h = setTimeout(() => setNeedle(text.trim()), 400);
    return () => clearTimeout(h);
  }, [text]);
  const s = useQuery({
    queryKey: ["kb-suggest", needle],
    queryFn: () => ok<ArticleRow[]>(api.GET("/api/kb/suggest", { params: { query: { q: needle } } })),
    enabled: needle.length > 3,
  });
  if (!s.data?.length) return null;
  return (
    <div className="rounded-lg border border-accent/30 bg-accentsoft/40 p-3 text-sm">
      <div className="mb-1 font-medium">{t("kb.maybeHelps")}</div>
      <ul className="space-y-1">
        {s.data.map((a) => (
          <li key={a.id}>
            <Link to="/kb/$id" params={{ id: a.id }} target="_blank" className="text-accent hover:underline">
              {a.title}
            </Link>
            {a.excerpt && <span className="block text-xs text-muted line-clamp-1">{a.excerpt}</span>}
          </li>
        ))}
      </ul>
    </div>
  );
}
