// Каталог услуг: категории заявок карточками по разделам; карточка открывает форму с этой категорией.
import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type Category } from "../../api/client";
import { Badge, Card, ErrorBox, Input, PageHeader, Spinner } from "../../ui";

type Section = { id: string; name: string; description: string; items: Category[] };

/** Разделы — категории верхнего уровня; карточки — их конечные подкатегории (или сам раздел без подкатегорий). */
function sections(cats: Category[]): Section[] {
  const children = new Map<string, Category[]>();
  for (const c of cats) {
    if (c.parent_id) children.set(c.parent_id, [...(children.get(c.parent_id) ?? []), c]);
  }
  const leaves = (c: Category): Category[] => {
    const kids = children.get(c.id) ?? [];
    return kids.length === 0 ? [c] : kids.flatMap(leaves);
  };
  return cats.filter((c) => !c.parent_id).map((c) => ({ id: c.id, name: c.name, description: c.description, items: leaves(c) }));
}

export function CatalogPage() {
  const { t } = useTranslation();
  const [q, setQ] = useState("");
  const cats = useQuery({ queryKey: ["categories"], queryFn: () => ok<Category[]>(api.GET("/api/helpdesk/categories")) });
  const list = useMemo(() => {
    const all = sections(cats.data ?? []);
    const needle = q.trim().toLowerCase();
    if (!needle) return all;
    const hit = (c: { name: string; description: string }) => `${c.name} ${c.description}`.toLowerCase().includes(needle);
    return all
      .map((s) => ({ ...s, items: hit(s) ? s.items : s.items.filter(hit) }))
      .filter((s) => s.items.length > 0);
  }, [cats.data, q]);
  return (
    <div className="mx-auto max-w-6xl">
      <PageHeader title={t("catalog.title")}>
        <Input type="search" aria-label={t("catalog.search")} placeholder={t("catalog.search")} value={q} onChange={(e) => setQ(e.target.value)} className="w-64" />
      </PageHeader>
      <p className="mb-4 text-sm text-muted">{t("catalog.hint")}</p>
      {cats.error ? (
        <ErrorBox error={cats.error} />
      ) : !cats.data ? (
        <Spinner />
      ) : list.length === 0 ? (
        <p className="text-sm text-muted">{t("catalog.empty")}</p>
      ) : (
        <div className="space-y-6">
          {list.map((s) => (
            <section key={s.id} aria-labelledby={`cat-${s.id}`}>
              <h2 id={`cat-${s.id}`} className="text-base font-semibold">
                {s.name}
              </h2>
              {s.description && s.items[0]?.id !== s.id && <p className="text-sm text-muted">{s.description}</p>}
              <ul className="mt-2 grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
                {s.items.map((c) => (
                  <li key={c.id}>
                    <Link to="/tickets/new" search={{ category: c.id }} className="block h-full rounded-xl focus-visible:outline-2 focus-visible:outline-accent">
                      <Card className="h-full p-4 transition-colors hover:bg-hover">
                        <div className="flex items-start gap-2">
                          <span className="font-medium">{c.name}</span>
                          <span className="ml-auto flex shrink-0 gap-1">
                            <Badge tone={c.kind === "request" ? "ok" : "muted"}>{c.kind === "request" ? t("catalog.request") : t("catalog.incident")}</Badge>
                          </span>
                        </div>
                        {c.description && <p className="mt-1 text-sm text-muted">{c.description}</p>}
                        {(c.form_schema as unknown as unknown[]).length > 0 && <p className="mt-2 text-xs text-muted">{t("catalog.withForm")}</p>}
                      </Card>
                    </Link>
                  </li>
                ))}
              </ul>
            </section>
          ))}
        </div>
      )}
    </div>
  );
}
