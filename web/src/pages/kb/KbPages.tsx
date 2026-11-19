// База знаний: поиск, разделы, статья, редактор (в том числе из решённой заявки).
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useParams, useSearch } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type Article, type ArticleRow, type KbSection } from "../../api/client";
import { formatDate } from "../../format";
import { can, useMe } from "../../session";
import { Badge, Button, Card, Empty, ErrorBox, Field, Input, PageHeader, Select, Spinner } from "../../ui";
import { RichText } from "../helpdesk/parts";

function useSections() {
  return useQuery({ queryKey: ["kb-sections"], queryFn: () => ok<KbSection[]>(api.GET("/api/kb/sections")) });
}

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
    <div className="mx-auto max-w-5xl">
      <PageHeader title={t("kb.title")}>
        {editor && (
          <Link to="/kb/new" search={{ ticket: undefined }} className="rounded-lg bg-ink px-3 py-1.5 text-sm font-medium text-paper">
            {t("kb.newArticle")}
          </Link>
        )}
      </PageHeader>
      <div className="grid gap-4 md:grid-cols-[14rem_1fr]">
        <nav className="space-y-1 text-sm">
          <button type="button" className={`block w-full rounded-lg px-3 py-1.5 text-left hover:bg-hover ${section === "" ? "bg-paper font-medium shadow-sm" : "text-muted"}`} onClick={() => setSection("")}>
            {t("kb.allSections")}
          </button>
          {sections.data?.map((s) => (
            <button key={s.id} type="button" className={`flex w-full rounded-lg px-3 py-1.5 text-left hover:bg-hover ${section === s.id ? "bg-paper font-medium shadow-sm" : "text-muted"}`} onClick={() => setSection(s.id)}>
              <span className="flex-1 truncate">{s.name}</span>
              <span className="text-xs">{s.articles}</span>
            </button>
          ))}
          {editor && <SectionEditor sections={sections.data ?? []} />}
        </nav>
        <div className="min-w-0">
          <Input type="search" className="mb-3" aria-label={t("kb.search")} placeholder={t("kb.search")} value={q} onChange={(e) => setQ(e.target.value)} />
          {list.error ? (
            <ErrorBox error={list.error} />
          ) : list.isPending ? (
            <Spinner />
          ) : list.data.length === 0 ? (
            <Empty>{t("kb.empty")}</Empty>
          ) : (
            <Card>
              <ul className="divide-y divide-line">
                {list.data.map((a) => (
                  <li key={a.id}>
                    <Link to="/kb/$id" params={{ id: a.id }} className="block px-4 py-3 hover:bg-hover">
                      <div className="flex flex-wrap items-center gap-2">
                        <span className="font-medium">{a.title}</span>
                        {a.status === "draft" && <Badge>{t("kb.draft")}</Badge>}
                        {a.audience === "engineers" && <Badge tone="accent">{t("kb.forEngineers")}</Badge>}
                        <span className="ml-auto text-xs text-muted">{formatDate(a.updated_at, i18n.language)}</span>
                      </div>
                      <div className="mt-1 line-clamp-2 text-sm text-muted">{a.excerpt}</div>
                    </Link>
                  </li>
                ))}
              </ul>
            </Card>
          )}
        </div>
      </div>
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
  return (
    <div className="mt-3 border-t border-line pt-3">
      <form
        className="flex gap-1"
        onSubmit={(e) => {
          e.preventDefault();
          if (name.trim()) add.mutate();
        }}
      >
        <Input value={name} onChange={(e) => setName(e.target.value)} placeholder={t("kb.newSection")} />
        <Button type="submit" disabled={!name.trim()}>
          +
        </Button>
      </form>
      {sections
        .filter((s) => s.articles === 0)
        .map((s) => (
          <button key={s.id} type="button" className="mt-1 block text-xs text-muted hover:text-bad" onClick={() => remove.mutate(s.id)}>
            ✕ {s.name}
          </button>
        ))}
      {(add.error || remove.error) && <ErrorBox error={add.error ?? remove.error} />}
    </div>
  );
}

export function KbArticlePage() {
  const { t, i18n } = useTranslation();
  const { id } = useParams({ from: "/kb/$id" });
  const a = useQuery({ queryKey: ["kb-article", id], queryFn: () => ok<Article>(api.GET("/api/kb/articles/{id}", { params: { path: { id } } })) });
  if (a.error) return <ErrorBox error={a.error} />;
  if (!a.data) return <Spinner />;
  const x = a.data;
  return (
    <div className="mx-auto max-w-3xl">
      <Link to="/kb" className="mb-2 inline-block text-sm text-accent hover:underline">
        ← {t("kb.title")}
      </Link>
      <PageHeader title={x.title}>
        {x.can_edit && (
          <Link to="/kb/$id/edit" params={{ id: x.id }} className="rounded-lg border border-line-strong px-3 py-1.5 text-sm">
            {t("common.edit")}
          </Link>
        )}
      </PageHeader>
      <div className="mb-3 flex flex-wrap gap-2 text-xs text-muted">
        {x.section_name && <Badge>{x.section_name}</Badge>}
        {x.status === "draft" && <Badge>{t("kb.draft")}</Badge>}
        {x.audience === "engineers" && <Badge tone="accent">{t("kb.forEngineers")}</Badge>}
        <span>
          {x.author_name} · {formatDate(x.updated_at, i18n.language)} · {t("kb.views", { n: x.views })}
        </span>
        {x.source_ticket_number && x.can_edit && (
          <Link to="/tickets/$number" params={{ number: String(x.source_ticket_number) }} className="text-accent">
            #{x.source_ticket_number}
          </Link>
        )}
      </div>
      <Card className="p-5">
        <RichText text={x.body} />
      </Card>
    </div>
  );
}

function ArticleForm({ initial, id }: { initial: { title: string; body: string; section_id: string; audience: string; status: string; source_ticket_number: number | null }; id?: string }) {
  const { t } = useTranslation();
  const nav = useNavigate();
  const qc = useQueryClient();
  const sections = useSections();
  const [f, setF] = useState(initial);
  const body = { ...f, section_id: f.section_id || null };
  const save = useMutation({
    mutationFn: async () => {
      if (id) {
        await ok(api.PUT("/api/kb/articles/{id}", { params: { path: { id } }, body }));
        return id;
      }
      const r = (await ok(api.POST("/api/kb/articles", { body }))) as { id: string };
      return r.id;
    },
    onSuccess: (to) => {
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
      void qc.invalidateQueries({ queryKey: ["kb"] });
      // Счётчики статей в разделах.
      void qc.invalidateQueries({ queryKey: ["kb-sections"] });
      void nav({ to: "/kb" });
    },
  });
  return (
    <div className="mx-auto max-w-3xl space-y-3">
      <PageHeader title={id ? t("kb.editArticle") : t("kb.newArticle")} />
      <Field label={t("kb.articleTitle")}>
        <Input value={f.title} onChange={(e) => setF({ ...f, title: e.target.value })} />
      </Field>
      <div className="grid gap-3 sm:grid-cols-3">
        <Field label={t("kb.section")}>
          <Select value={f.section_id} onChange={(e) => setF({ ...f, section_id: e.target.value })}>
            <option value="">—</option>
            {sections.data?.map((s) => (
              <option key={s.id} value={s.id}>
                {s.name}
              </option>
            ))}
          </Select>
        </Field>
        <Field label={t("kb.audience")}>
          <Select value={f.audience} onChange={(e) => setF({ ...f, audience: e.target.value })}>
            <option value="all">{t("kb.audienceAll")}</option>
            <option value="engineers">{t("kb.forEngineers")}</option>
          </Select>
        </Field>
        <Field label={t("kb.status")}>
          <Select value={f.status} onChange={(e) => setF({ ...f, status: e.target.value })}>
            <option value="draft">{t("kb.draft")}</option>
            <option value="published">{t("kb.published")}</option>
          </Select>
        </Field>
      </div>
      <Field label={t("kb.body")} hint={t("kb.markdownHint")}>
        <textarea rows={16} value={f.body} onChange={(e) => setF({ ...f, body: e.target.value })} className="w-full rounded-lg border border-control bg-paper px-3 py-2 font-mono text-sm" />
      </Field>
      {(save.error || remove.error) && <ErrorBox error={save.error ?? remove.error} />}
      <div className="flex gap-2">
        <Button variant="primary" disabled={!f.title.trim() || !f.body.trim() || save.isPending} onClick={() => save.mutate()}>
          {t("common.save")}
        </Button>
        {id && (
          <Button variant="danger" className="ml-auto" onClick={() => window.confirm(t("kb.deleteConfirm")) && remove.mutate()}>
            {t("common.delete")}
          </Button>
        )}
      </div>
    </div>
  );
}

export function KbNewPage() {
  const { ticket } = useSearch({ from: "/kb/new" });
  const draft = useQuery({
    queryKey: ["kb-draft", ticket],
    queryFn: () => ok<{ title: string; body: string; source_ticket_number: number }>(api.GET("/api/helpdesk/tickets/{number}/kb-draft", { params: { path: { number: ticket ?? 0 } } })),
    enabled: !!ticket,
  });
  if (ticket && draft.error) return <ErrorBox error={draft.error} />;
  if (ticket && !draft.data) return <Spinner />;
  return (
    <ArticleForm
      initial={{ title: draft.data?.title ?? "", body: draft.data?.body ?? "", section_id: "", audience: "all", status: "draft", source_ticket_number: draft.data?.source_ticket_number ?? null }}
    />
  );
}

export function KbEditPage() {
  const { id } = useParams({ from: "/kb/$id/edit" });
  const a = useQuery({ queryKey: ["kb-article", id], queryFn: () => ok<Article>(api.GET("/api/kb/articles/{id}", { params: { path: { id } } })) });
  if (a.error) return <ErrorBox error={a.error} />;
  if (!a.data) return <Spinner />;
  const x = a.data;
  return <ArticleForm id={id} initial={{ title: x.title, body: x.body, section_id: x.section_id ?? "", audience: x.audience, status: x.status, source_ticket_number: x.source_ticket_number ?? null }} />;
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
          </li>
        ))}
      </ul>
    </div>
  );
}
