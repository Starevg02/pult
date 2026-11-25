// CMDB в карточке объекта: связи, анализ влияния, карта зависимостей. Лицензии ПО. Жизненный цикл типа.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type Asset, type AssetMap, type AssetRelation, type ImpactedAsset, type License, type LicenseDetail, type Lifecycle, type RelationSuggestion, type RelationType } from "../../api/client";
import { formatDate } from "../../format";
import { can, useMe } from "../../session";
import { Badge, Button, Card, Dialog, Empty, ErrorBox, Field, Input, PageHeader, Select, Spinner } from "../../ui";
import { dyn } from "../helpdesk/helpdeskUtil";
import { ASSET_STATUSES } from "./cmdbUtil";


function useRelationTypes() {
  return useQuery({ queryKey: ["asset-relation-types"], queryFn: () => ok<RelationType[]>(api.GET("/api/helpdesk/asset-relation-types")) });
}

/** Поиск объекта каталога для связи: сначала предложения портала, затем поиск по названию. */
function RelationDialog({ asset, onClose }: { asset: Asset; onClose: () => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const types = useRelationTypes();
  const [kind, setKind] = useState("");
  const [q, setQ] = useState("");
  const [needle, setNeedle] = useState("");
  const [other, setOther] = useState<{ id: string; name: string } | null>(null);
  useEffect(() => {
    const h = setTimeout(() => setNeedle(q.trim()), 250);
    return () => clearTimeout(h);
  }, [q]);
  const suggestions = useQuery({
    queryKey: ["asset-suggestions", asset.id],
    queryFn: () => ok<RelationSuggestion[]>(api.GET("/api/helpdesk/assets/{id}/relation-suggestions", { params: { path: { id: asset.id } } })),
  });
  const found = useQuery({
    queryKey: ["assets", needle, "", false],
    enabled: needle.length >= 2,
    queryFn: () => ok<Asset[]>(api.GET("/api/helpdesk/assets", { params: { query: { q: needle } } })),
  });
  const active = (types.data ?? []).filter((x) => x.is_active);
  const choice = kind || (active[0] ? `${active[0].id}:out` : "");
  const [typeId = "", direction = "out"] = choice.split(":");
  const add = useMutation({
    mutationFn: () => ok(api.POST("/api/helpdesk/assets/{id}/relations", { params: { path: { id: asset.id } }, body: { type_id: typeId, other_id: other?.id ?? "", direction } })),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["asset-relations", asset.id] });
      void qc.invalidateQueries({ queryKey: ["asset-suggestions", asset.id] });
      void qc.invalidateQueries({ queryKey: ["asset-map", asset.id] });
      void qc.invalidateQueries({ queryKey: ["asset-impact", asset.id] });
      onClose();
    },
  });
  const pickRow = (id: string, name: string, detail: string) => (
    <li key={id}>
      <button type="button" aria-pressed={other?.id === id} className={`flex w-full flex-wrap items-center gap-2 px-3 py-1.5 text-left text-sm hover:bg-hover ${other?.id === id ? "bg-accentsoft" : ""}`} onClick={() => setOther({ id, name })}>
        <span className="font-medium">{name}</span>
        <span className="text-xs text-muted">{detail}</span>
      </button>
    </li>
  );
  const results = (found.data ?? []).filter((a) => a.id !== asset.id);
  return (
    <Dialog open onClose={onClose} title={t("cmdb.addRelation")} wide>
      <div className="space-y-3">
        <Field label={t("cmdb.relation")}>
          <Select value={choice} onChange={(e) => setKind(e.target.value)}>
            {active.map((x) => [
              <option key={`${x.id}:out`} value={`${x.id}:out`}>
                {asset.name} — {x.name} — …
              </option>,
              <option key={`${x.id}:in`} value={`${x.id}:in`}>
                {asset.name} — {x.reverse_name} — …
              </option>,
            ])}
          </Select>
        </Field>
        {(suggestions.data?.length ?? 0) > 0 && (
          <div>
            <div className="mb-1 text-sm font-medium">{t("cmdb.suggested")}</div>
            <p className="mb-1 text-xs text-muted">{t("cmdb.suggestedHint")}</p>
            <Card>
              <ul className="max-h-40 divide-y divide-line overflow-y-auto">{(suggestions.data ?? []).map((s) => pickRow(s.id, s.name, `${s.type_name} · ${dyn(t, `cmdb.reasons.${s.reason}`)}`))}</ul>
            </Card>
          </div>
        )}
        <Field label={t("cmdb.findAsset")}>
          <Input type="search" value={q} onChange={(e) => setQ(e.target.value)} placeholder={t("assets.search")} />
        </Field>
        {results.length > 0 && (
          <Card>
            <ul className="max-h-48 divide-y divide-line overflow-y-auto">{results.slice(0, 30).map((a) => pickRow(a.id, a.name, [a.type_name, a.inventory_number].filter(Boolean).join(" · ")))}</ul>
          </Card>
        )}
        {needle.length >= 2 && found.data && results.length === 0 && <p className="text-sm text-muted">{t("assets.empty")}</p>}
        {other && (
          <p className="rounded-lg bg-sunken px-3 py-2 text-sm" role="status">
            {asset.name} — {direction === "in" ? active.find((x) => x.id === typeId)?.reverse_name : active.find((x) => x.id === typeId)?.name} — <b>{other.name}</b>
          </p>
        )}
        {add.error && <ErrorBox error={add.error} />}
        <div className="flex justify-end gap-2">
          <Button onClick={onClose}>{t("common.cancel")}</Button>
          <Button variant="primary" disabled={!other || !typeId || add.isPending} onClick={() => add.mutate()}>
            {t("common.add")}
          </Button>
        </div>
      </div>
    </Dialog>
  );
}

/** Связи объекта с другими объектами CMDB. */
export function AssetRelations({ asset }: { asset: Asset }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const manage = can(useMe().data, "helpdesk.assets.manage");
  const [adding, setAdding] = useState(false);
  const rel = useQuery({
    queryKey: ["asset-relations", asset.id],
    queryFn: () => ok<AssetRelation[]>(api.GET("/api/helpdesk/assets/{id}/relations", { params: { path: { id: asset.id } } })),
  });
  const remove = useMutation({
    mutationFn: (r: AssetRelation) => ok(api.DELETE("/api/helpdesk/assets/{id}/relations/{relation}", { params: { path: { id: asset.id, relation: r.id } } })),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["asset-relations", asset.id] });
      void qc.invalidateQueries({ queryKey: ["asset-map", asset.id] });
      void qc.invalidateQueries({ queryKey: ["asset-impact", asset.id] });
    },
  });
  return (
    <section className="mb-4" aria-labelledby="cmdb-relations">
      <div className="mb-2 flex items-center gap-2">
        <h2 id="cmdb-relations" className="font-medium">
          {t("cmdb.relations")}
        </h2>
        {manage && (
          <Button variant="ghost" className="ml-auto min-h-7 px-2 text-xs" onClick={() => setAdding(true)}>
            {t("cmdb.addRelation")}
          </Button>
        )}
      </div>
      {rel.error ? (
        <ErrorBox error={rel.error} />
      ) : rel.isPending ? (
        <Spinner />
      ) : rel.data.length === 0 ? (
        <p className="text-sm text-muted">{t("cmdb.noRelations")}</p>
      ) : (
        <Card>
          <ul className="divide-y divide-line">
            {rel.data.map((r) => (
              <li key={r.id} className="flex flex-wrap items-center gap-2 px-4 py-2 text-sm">
                <span className="w-40 shrink-0 text-muted">{r.label}</span>
                <Link to="/objects/$kind/$id" params={{ kind: "asset", id: r.other_id }} className="font-medium text-accent hover:underline">
                  {r.other_name}
                </Link>
                <span className="text-xs text-muted">{r.other_type}</span>
                {r.other_status === "retired" && <Badge>{dyn(t, "assets.statuses.retired")}</Badge>}
                {manage && (
                  <Button variant="ghost" className="ml-auto min-h-7 px-2 text-xs" aria-label={t("cmdb.removeRelation", { name: r.other_name })} disabled={remove.isPending} onClick={() => remove.mutate(r)}>
                    ✕
                  </Button>
                )}
              </li>
            ))}
          </ul>
        </Card>
      )}
      {remove.error && <ErrorBox error={remove.error} />}
      {adding && <RelationDialog asset={asset} onClose={() => setAdding(false)} />}
      <ImpactAndMap asset={asset} />
    </section>
  );
}

function ImpactAndMap({ asset }: { asset: Asset }) {
  const { t } = useTranslation();
  const [view, setView] = useState<"" | "impact" | "map">("");
  return (
    <div className="mt-2">
      <div className="flex flex-wrap gap-2" role="group" aria-label={t("cmdb.analysis")}>
        <Button variant="ghost" className="min-h-7 px-2 text-xs" aria-pressed={view === "impact"} onClick={() => setView(view === "impact" ? "" : "impact")}>
          {t("cmdb.impact")}
        </Button>
        <Button variant="ghost" className="min-h-7 px-2 text-xs" aria-pressed={view === "map"} onClick={() => setView(view === "map" ? "" : "map")}>
          {t("cmdb.map")}
        </Button>
      </div>
      {view === "impact" && <Impact asset={asset} />}
      {view === "map" && <DependencyMap asset={asset} />}
    </div>
  );
}

/** Что затронет отказ объекта. */
function Impact({ asset }: { asset: Asset }) {
  const { t } = useTranslation();
  const q = useQuery({
    queryKey: ["asset-impact", asset.id],
    queryFn: () => ok<ImpactedAsset[]>(api.GET("/api/helpdesk/assets/{id}/impact", { params: { path: { id: asset.id } } })),
  });
  if (q.error) return <ErrorBox error={q.error} />;
  if (q.isPending) return <Spinner />;
  if (q.data.length === 0) return <p className="mt-2 text-sm text-muted">{t("cmdb.noImpact")}</p>;
  const people = new Set(q.data.map((x) => x.responsible_name).filter(Boolean)).size;
  return (
    <Card className="mt-2 overflow-x-auto">
      <p className="px-4 pt-3 text-sm" role="status">
        {t("cmdb.impactSummary", { n: q.data.length, people })}
      </p>
      <table className="w-full text-sm">
        <thead className="text-left text-xs text-muted">
          <tr className="border-b border-line">
            <th className="px-3 py-2">{t("assets.name")}</th>
            <th className="px-3 py-2">{t("cmdb.via")}</th>
            <th className="hidden px-3 py-2 md:table-cell">{t("assets.responsible")}</th>
            <th className="px-3 py-2">{t("assets.openTickets")}</th>
          </tr>
        </thead>
        <tbody className="divide-y divide-line">
          {q.data.map((x) => (
            <tr key={x.id}>
              <td className="px-3 py-2">
                <Link to="/objects/$kind/$id" params={{ kind: "asset", id: x.id }} className="font-medium text-accent hover:underline">
                  {x.name}
                </Link>
                <span className="ml-2 text-xs text-muted">{x.type_name}</span>
              </td>
              <td className="px-3 py-2 text-muted">
                {x.relation} {x.via_name}
                {x.depth > 1 && <span className="ml-1 text-xs">({t("cmdb.level", { n: x.depth })})</span>}
              </td>
              <td className="hidden px-3 py-2 text-muted md:table-cell">{x.responsible_name}</td>
              <td className="px-3 py-2">{x.open_tickets > 0 ? <Badge tone="accent">{x.open_tickets}</Badge> : ""}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </Card>
  );
}

const W = 760;
const H = 560;
const short = (s: string) => (s.length > 22 ? `${s.slice(0, 21)}…` : s);

/** Карта зависимостей: объект в центре, соседи — на кольцах по числу шагов. */
function DependencyMap({ asset }: { asset: Asset }) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const q = useQuery({
    queryKey: ["asset-map", asset.id],
    queryFn: () => ok<AssetMap>(api.GET("/api/helpdesk/assets/{id}/map", { params: { path: { id: asset.id } } })),
  });
  if (q.error) return <ErrorBox error={q.error} />;
  if (q.isPending) return <Spinner />;
  const { nodes, edges } = q.data;
  if (nodes.length <= 1) return <p className="mt-2 text-sm text-muted">{t("cmdb.noRelations")}</p>;
  const pos = new Map<string, { x: number; y: number }>();
  for (const d of [0, 1, 2]) {
    const ring = nodes.filter((n) => n.distance === d);
    const r = d === 0 ? 0 : d === 1 ? 150 : 250;
    ring.forEach((n, i) => {
      const a = (2 * Math.PI * i) / Math.max(ring.length, 1) - Math.PI / 2 + (d === 2 ? Math.PI / Math.max(ring.length, 1) : 0);
      pos.set(n.id, { x: W / 2 + r * Math.cos(a) * 1.3, y: H / 2 + r * Math.sin(a) });
    });
  }
  const open = (id: string) => void navigate({ to: "/objects/$kind/$id", params: { kind: "asset", id } });
  return (
    <Card className="mt-2 overflow-x-auto p-2">
      <svg viewBox={`0 0 ${W} ${H}`} className="h-auto w-full min-w-[560px]" role="img" aria-label={t("cmdb.mapLabel", { name: asset.name, n: nodes.length - 1 })}>
        <defs>
          <marker id="cmdb-arrow" viewBox="0 0 10 10" refX="10" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
            <path d="M0,0 L10,5 L0,10 z" className="fill-muted" />
          </marker>
        </defs>
        {edges.map((e, i) => {
          const a = pos.get(e.from_id);
          const b = pos.get(e.to_id);
          if (!a || !b) return null;
          // Стрелка останавливается у края подписи второго объекта.
          const dx = b.x - a.x;
          const dy = b.y - a.y;
          const len = Math.hypot(dx, dy) || 1;
          const end = { x: b.x - (dx / len) * 34, y: b.y - (dy / len) * 18 };
          return (
            <g key={i}>
              <line x1={a.x} y1={a.y} x2={end.x} y2={end.y} className={e.impact ? "stroke-accent" : "stroke-line"} strokeWidth={1.5} strokeDasharray={e.impact ? undefined : "4 3"} markerEnd="url(#cmdb-arrow)" />
              <text x={(a.x + b.x) / 2} y={(a.y + b.y) / 2 - 4} textAnchor="middle" className="fill-muted text-[10px]">
                {short(e.name)}
              </text>
            </g>
          );
        })}
        {nodes.map((n) => {
          const p = pos.get(n.id) ?? { x: W / 2, y: H / 2 };
          const center = n.distance === 0;
          return (
            <g
              key={n.id}
              role={center ? undefined : "link"}
              tabIndex={center ? undefined : 0}
              aria-label={center ? undefined : `${n.name}, ${n.type_name}`}
              className={center ? "" : "cursor-pointer focus:outline-none [&:focus_rect]:stroke-accent [&:hover_rect]:stroke-accent"}
              onClick={center ? undefined : () => open(n.id)}
              onKeyDown={
                center
                  ? undefined
                  : (ev) => {
                      if (ev.key === "Enter" || ev.key === " ") {
                        ev.preventDefault();
                        open(n.id);
                      }
                    }
              }
            >
              <rect x={p.x - 80} y={p.y - 18} width={160} height={36} rx={8} className={`${center ? "fill-accentsoft stroke-accent" : "fill-paper stroke-line"} ${n.status === "retired" ? "opacity-60" : ""}`} strokeWidth={1.5} />
              <text x={p.x} y={p.y - 2} textAnchor="middle" className="fill-ink text-[12px] font-medium">
                {short(n.name)}
              </text>
              <text x={p.x} y={p.y + 12} textAnchor="middle" className="fill-muted text-[10px]">
                {short(n.type_name)}
              </text>
            </g>
          );
        })}
      </svg>
      <p className="px-2 pb-1 text-xs text-muted">
        {t("cmdb.mapLegend")}
        {q.data.truncated && ` ${t("cmdb.mapTruncated")}`}
      </p>
    </Card>
  );
}

// ---------- Жизненный цикл типа ----------

/** Состояния типа и разрешённые переходы. Пустые переходы — любые. */
export function LifecycleEditor({ value, onChange }: { value: Lifecycle; onChange: (v: Lifecycle) => void }) {
  const { t } = useTranslation();
  const required = (s: string) => s === "in_use" || s === "in_stock";
  const moves = value.transitions ?? [];
  const has = (a: string, b: string) => moves.some(([x, y]) => x === a && y === b);
  const toggleStatus = (s: string, on: boolean) => {
    const statuses = ASSET_STATUSES.filter((x) => (x === s ? on : value.statuses.includes(x)));
    onChange({ statuses, transitions: moves.filter(([a, b]) => statuses.includes(a as never) && statuses.includes(b as never)) });
  };
  const toggleMove = (a: string, b: string, on: boolean) =>
    onChange({ ...value, transitions: on ? [...moves, [a, b]] : moves.filter(([x, y]) => !(x === a && y === b)) });
  const label = (s: string) => dyn(t, `assets.statuses.${s}`);
  return (
    <fieldset className="space-y-2">
      <legend className="text-sm font-medium">{t("cmdb.lifecycle")}</legend>
      <div className="flex flex-wrap gap-4 text-sm">
        {ASSET_STATUSES.map((s) => (
          <label key={s} className="inline-flex items-center gap-2">
            <input type="checkbox" checked={value.statuses.includes(s)} disabled={required(s)} onChange={(e) => toggleStatus(s, e.target.checked)} />
            {label(s)}
          </label>
        ))}
      </div>
      <p className="text-xs text-muted">{t("cmdb.transitionsHint")}</p>
      <div className="overflow-x-auto">
        <table className="text-sm">
          <caption className="sr-only">{t("cmdb.transitions")}</caption>
          <thead>
            <tr>
              <th className="px-2 py-1 text-left text-xs text-muted">{t("cmdb.fromTo")}</th>
              {value.statuses.map((b) => (
                <th key={b} scope="col" className="px-2 py-1 text-xs font-normal text-muted">
                  {label(b)}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {value.statuses.map((a) => (
              <tr key={a}>
                <th scope="row" className="px-2 py-1 text-left text-xs font-normal text-muted">
                  {label(a)}
                </th>
                {value.statuses.map((b) => (
                  <td key={b} className="px-2 py-1 text-center">
                    {a === b ? (
                      "—"
                    ) : (
                      <input type="checkbox" aria-label={t("cmdb.transition", { from: label(a), to: label(b) })} checked={has(a, b)} onChange={(e) => toggleMove(a, b, e.target.checked)} />
                    )}
                  </td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </fieldset>
  );
}

// ---------- Лицензии ----------

const today = () => new Date().toISOString().slice(0, 10);
const soon = (d: string) => {
  const x = new Date(`${d}T00:00:00`);
  x.setDate(x.getDate() - 30);
  return x.toISOString().slice(0, 10) <= today();
};

function LicenseState({ l }: { l: License }) {
  const { t } = useTranslation();
  return (
    <span className="inline-flex flex-wrap gap-1">
      {l.seats != null && l.installed > l.seats && <Badge tone="bad">{t("cmdb.overSeats", { n: l.installed - l.seats })}</Badge>}
      {l.expires_on && l.expires_on < today() && <Badge tone="bad">{t("cmdb.expired")}</Badge>}
      {l.expires_on && l.expires_on >= today() && soon(l.expires_on) && <Badge tone="accent">{t("cmdb.expiresSoon")}</Badge>}
      {!l.is_active && <Badge>{t("admin.helpdesk.inactive")}</Badge>}
    </span>
  );
}

type LicenseDraft = { name: string; vendor: string; pattern: string; seats: string; expires_on: string; notes: string; is_active: boolean };

function LicenseDialog({ license, onClose }: { license: License | null; onClose: () => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [f, setF] = useState<LicenseDraft>(() => ({
    name: license?.name ?? "",
    vendor: license?.vendor ?? "",
    pattern: license?.pattern ?? "",
    seats: license?.seats != null ? String(license.seats) : "",
    expires_on: license?.expires_on ?? "",
    notes: license?.notes ?? "",
    is_active: license?.is_active ?? true,
  }));
  const body = {
    name: f.name,
    vendor: f.vendor || null,
    pattern: f.pattern,
    seats: f.seats === "" ? null : Number(f.seats),
    expires_on: f.expires_on || null,
    notes: f.notes || null,
    is_active: f.is_active,
  };
  const done = () => {
    void qc.invalidateQueries({ queryKey: ["licenses"] });
    onClose();
  };
  const save = useMutation({
    mutationFn: () => (license ? ok(api.PUT("/api/helpdesk/licenses/{id}", { params: { path: { id: license.id } }, body })) : ok(api.POST("/api/helpdesk/licenses", { body }))),
    onSuccess: done,
  });
  const del = useMutation({ mutationFn: () => ok(api.DELETE("/api/helpdesk/licenses/{id}", { params: { path: { id: license?.id ?? "" } } })), onSuccess: done });
  return (
    <Dialog open onClose={onClose} title={license ? license.name : t("cmdb.newLicense")} wide>
      <div className="grid gap-3 sm:grid-cols-2">
        <Field label={t("cmdb.licenseName")}>
          <Input value={f.name} maxLength={200} onChange={(e) => setF({ ...f, name: e.target.value })} />
        </Field>
        <Field label={t("cmdb.vendor")}>
          <Input value={f.vendor} maxLength={200} onChange={(e) => setF({ ...f, vendor: e.target.value })} />
        </Field>
        <div className="sm:col-span-2">
          <Field label={t("cmdb.pattern")} hint={t("cmdb.patternHint")}>
            <Input value={f.pattern} maxLength={200} onChange={(e) => setF({ ...f, pattern: e.target.value })} placeholder="Microsoft Office" />
          </Field>
        </div>
        <Field label={t("cmdb.seats")} hint={t("cmdb.seatsHint")}>
          <Input inputMode="numeric" value={f.seats} onChange={(e) => setF({ ...f, seats: e.target.value.replace(/\D/g, "") })} />
        </Field>
        <Field label={t("cmdb.expiresOn")}>
          <Input type="date" value={f.expires_on} onChange={(e) => setF({ ...f, expires_on: e.target.value })} />
        </Field>
        <div className="sm:col-span-2">
          <Field label={t("assets.notes")}>
            <textarea rows={3} maxLength={2000} value={f.notes} onChange={(e) => setF({ ...f, notes: e.target.value })} className="w-full rounded-lg border border-control bg-paper px-3 py-2 text-sm" />
          </Field>
        </div>
        <label className="inline-flex items-center gap-2 text-sm">
          <input type="checkbox" checked={f.is_active} onChange={(e) => setF({ ...f, is_active: e.target.checked })} />
          {t("admin.helpdesk.active")}
        </label>
      </div>
      {(save.error || del.error) && <ErrorBox error={save.error ?? del.error} />}
      <div className="mt-3 flex flex-wrap justify-end gap-2">
        {license && (
          <Button variant="danger" className="mr-auto" disabled={del.isPending} onClick={() => window.confirm(t("cmdb.deleteLicense")) && del.mutate()}>
            {t("common.delete")}
          </Button>
        )}
        <Button onClick={onClose}>{t("common.cancel")}</Button>
        <Button variant="primary" disabled={!f.name.trim() || f.pattern.trim().length < 2 || save.isPending} onClick={() => save.mutate()}>
          {t("common.save")}
        </Button>
      </div>
    </Dialog>
  );
}

function LicenseDevices({ id }: { id: string }) {
  const { t, i18n } = useTranslation();
  const q = useQuery({ queryKey: ["license", id], queryFn: () => ok<LicenseDetail>(api.GET("/api/helpdesk/licenses/{id}", { params: { path: { id } } })) });
  if (q.error) return <ErrorBox error={q.error} />;
  if (q.isPending) return <Spinner />;
  if (q.data.devices.length === 0) return <p className="px-4 py-2 text-sm text-muted">{t("cmdb.notInstalled")}</p>;
  return (
    <ul className="grid gap-x-4 gap-y-1 px-4 py-2 text-sm sm:grid-cols-2">
      {q.data.devices.map((d) => (
        <li key={d.device_id} className="flex flex-wrap gap-2">
          <span className="font-mono text-xs">{d.hostname}</span>
          <span className="text-xs text-muted">
            {d.program} {d.version}
            {d.last_seen_at && ` · ${formatDate(d.last_seen_at, i18n.language)}`}
          </span>
        </li>
      ))}
    </ul>
  );
}

/** Лицензии ПО: куплено мест и сколько установок нашли агенты. */
export function LicensesPage() {
  const { t } = useTranslation();
  const manage = can(useMe().data, "helpdesk.assets.manage");
  const [edit, setEdit] = useState<License | "new" | null>(null);
  const [open, setOpen] = useState<string | null>(null);
  const list = useQuery({ queryKey: ["licenses"], queryFn: () => ok<License[]>(api.GET("/api/helpdesk/licenses")) });
  return (
    <div className="mx-auto max-w-5xl">
      <Link to="/assets" className="mb-2 inline-block text-sm text-accent hover:underline">
        ← {t("assets.title")}
      </Link>
      <PageHeader title={t("cmdb.licenses")} subtitle={t("cmdb.licensesHint")}>
        {manage && (
          <Button variant="primary" onClick={() => setEdit("new")}>
            {t("cmdb.newLicense")}
          </Button>
        )}
      </PageHeader>
      {list.error ? (
        <ErrorBox error={list.error} />
      ) : list.isPending ? (
        <Spinner />
      ) : list.data.length === 0 ? (
        <Empty>{t("cmdb.noLicenses")}</Empty>
      ) : (
        <Card>
          <ul className="divide-y divide-line">
            {list.data.map((l) => (
              <li key={l.id}>
                <div className="flex flex-wrap items-center gap-3 px-4 py-2 text-sm">
                  <button type="button" className="min-w-0 flex-1 text-left" aria-expanded={open === l.id} onClick={() => setOpen(open === l.id ? null : l.id)}>
                    <span className="font-medium">{l.name}</span>
                    {l.vendor && <span className="ml-2 text-xs text-muted">{l.vendor}</span>}
                    <span className="block text-xs text-muted">«{l.pattern}»</span>
                  </button>
                  <span className="tabular-nums">{l.seats != null ? t("cmdb.usage", { used: l.installed, seats: l.seats }) : t("cmdb.usageUnlimited", { used: l.installed })}</span>
                  {l.expires_on && <span className="text-xs text-muted">{t("cmdb.until", { date: l.expires_on })}</span>}
                  <LicenseState l={l} />
                  {manage && (
                    <Button variant="ghost" className="min-h-7 px-2 text-xs" onClick={() => setEdit(l)}>
                      {t("common.edit")}
                    </Button>
                  )}
                </div>
                {open === l.id && <LicenseDevices id={l.id} />}
              </li>
            ))}
          </ul>
        </Card>
      )}
      {edit && <LicenseDialog license={edit === "new" ? null : edit} onClose={() => setEdit(null)} />}
    </div>
  );
}
