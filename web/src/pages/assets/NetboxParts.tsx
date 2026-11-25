// Инвентарь из NetBox: проверка найденного инженером и настройки подключения (суперадминистратор).
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type AssetType, type ExternalItem, type NetboxState, type NetboxView, type Person, type Site } from "../../api/client";
import { formatDate } from "../../format";
import { Button, Card, Dialog, Empty, ErrorBox, Field, Input, Select, Spinner } from "../../ui";
import { fullName } from "../phonebookUtil";
import { PersonPicker } from "../helpdesk/NewTicketPage";
import { dyn } from "../helpdesk/helpdeskUtil";
import { statusesOf } from "./cmdbUtil";

type FormField = { key: string };

function AcceptExternal({ item, onClose }: { item: ExternalItem; onClose: () => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const types = useQuery({ queryKey: ["asset-types"], queryFn: () => ok<AssetType[]>(api.GET("/api/helpdesk/asset-types")) });
  const sites = useQuery({ queryKey: ["sites"], queryFn: () => ok<Site[]>(api.GET("/api/sites")) });
  const [typeId, setTypeId] = useState("");
  const [name, setName] = useState(item.name);
  const [inventory, setInventory] = useState("");
  const [site, setSite] = useState("");
  const [status, setStatus] = useState("in_use");
  const [person, setPerson] = useState<Person | null>(null);
  const guess = types.data?.find((x) => ((x.fields ?? []) as FormField[]).some((f) => f.key === "ip")) ?? types.data?.[0];
  const type = typeId || guess?.id || "";
  const statuses = statusesOf(types.data?.find((x) => x.id === type));
  const save = useMutation({
    mutationFn: () =>
      ok(
        api.POST("/api/helpdesk/inventory/external/{id}/accept", {
          params: { path: { id: item.id } },
          body: { type_id: type, name: name.trim() || null, inventory_number: inventory.trim() || null, site_id: site || null, responsible_person_id: person?.id ?? null, status },
        }),
      ),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["external-inventory"] });
      void qc.invalidateQueries({ queryKey: ["assets"] });
      void qc.invalidateQueries({ queryKey: ["asset-types"] });
      onClose();
    },
  });
  return (
    <Dialog open onClose={onClose} title={`${t("assets.found.accept")}: ${item.name}`}>
      <div className="grid gap-3">
        <Field label={t("assets.type")}>
          <Select value={type} onChange={(e) => setTypeId(e.target.value)}>
            {types.data?.map((x) => (
              <option key={x.id} value={x.id}>
                {x.name}
              </option>
            ))}
          </Select>
        </Field>
        <Field label={t("assets.name")}>
          <Input value={name} onChange={(e) => setName(e.target.value)} />
        </Field>
        <Field label={t("assets.inventory")}>
          <Input value={inventory} onChange={(e) => setInventory(e.target.value)} />
        </Field>
        <Field label={t("assets.status")}>
          <Select value={statuses.includes(status) ? status : "in_use"} onChange={(e) => setStatus(e.target.value)}>
            {statuses.map((s) => (
              <option key={s} value={s}>
                {dyn(t, `assets.statuses.${s}`)}
              </option>
            ))}
          </Select>
        </Field>
        <Field label={t("helpdesk.site")}>
          <Select value={site} onChange={(e) => setSite(e.target.value)}>
            <option value="">—</option>
            {sites.data?.map((s) => (
              <option key={s.id} value={s.id}>
                {s.name}
              </option>
            ))}
          </Select>
        </Field>
        <Field group label={t("assets.responsible")}>
          {person ? (
            <div className="flex items-center gap-2 text-sm">
              <span className="flex-1">{fullName(person)}</span>
              <Button variant="ghost" onClick={() => setPerson(null)}>
                {t("common.change")}
              </Button>
            </div>
          ) : (
            <PersonPicker value={null} onPick={setPerson} />
          )}
        </Field>
      </div>
      {save.error && <ErrorBox error={save.error} />}
      <div className="mt-3 flex justify-end gap-2">
        <Button onClick={onClose}>{t("common.cancel")}</Button>
        <Button variant="primary" disabled={!type || save.isPending} onClick={() => save.mutate()}>
          {t("assets.found.accept")}
        </Button>
      </div>
    </Dialog>
  );
}

/** Найденное в NetBox, чего нет в каталоге: принять или отклонить. */
export function ExternalFound() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [open, setOpen] = useState(false);
  const [rejected, setRejected] = useState(false);
  const [accept, setAccept] = useState<ExternalItem | null>(null);
  const fetchList = (r: boolean) => ok<ExternalItem[]>(api.GET("/api/helpdesk/inventory/external", { params: { query: { rejected: r || undefined } } }));
  const pending = useQuery({ queryKey: ["external-inventory", false], queryFn: () => fetchList(false) });
  const declined = useQuery({ queryKey: ["external-inventory", true], queryFn: () => fetchList(true), enabled: open && rejected });
  const act = useMutation({
    mutationFn: ({ id, restore }: { id: string; restore: boolean }) =>
      ok(restore ? api.POST("/api/helpdesk/inventory/external/{id}/restore", { params: { path: { id } } }) : api.POST("/api/helpdesk/inventory/external/{id}/reject", { params: { path: { id } } })),
    onSuccess: () => void qc.invalidateQueries({ queryKey: ["external-inventory"] }),
  });
  if (!pending.data || (pending.data.length === 0 && !open)) return null;
  const list = rejected ? declined : pending;
  return (
    <Card className="mb-3">
      <button type="button" className="flex w-full items-center justify-between gap-2 px-4 py-2 text-left text-sm" aria-expanded={open} onClick={() => setOpen(!open)}>
        <span>{t("netbox.summary", { n: pending.data.length })}</span>
        <span className="text-accent">{open ? t("assets.found.hide") : t("assets.found.review")}</span>
      </button>
      {open && (
        <div className="border-t border-line">
          {act.error && <ErrorBox error={act.error} />}
          {list.error ? (
            <ErrorBox error={list.error} />
          ) : !list.data ? (
            <Spinner />
          ) : list.data.length === 0 ? (
            <Empty>{t("assets.found.empty")}</Empty>
          ) : (
            <div className="overflow-x-auto">
              <table className="w-full text-sm">
                <thead className="text-left text-xs text-muted">
                  <tr className="border-b border-line">
                    <th className="px-3 py-2">{t("assets.name")}</th>
                    <th className="hidden px-3 py-2 md:table-cell">{t("netbox.role")}</th>
                    <th className="hidden px-3 py-2 sm:table-cell">IP</th>
                    <th className="hidden px-3 py-2 lg:table-cell">{t("helpdesk.site")}</th>
                    <th className="px-3 py-2" />
                  </tr>
                </thead>
                <tbody className="divide-y divide-line">
                  {list.data.map((d) => (
                    <tr key={d.id}>
                      <td className="px-3 py-2 font-medium">
                        {d.name}
                        <div className="text-xs font-normal text-muted">
                          {dyn(t, `netbox.kinds.${d.kind}`)}
                          {[d.manufacturer, d.model].filter(Boolean).length > 0 && ` · ${[d.manufacturer, d.model].filter(Boolean).join(" ")}`}
                          {d.serial_number && <span className="font-mono"> · {d.serial_number}</span>}
                        </div>
                      </td>
                      <td className="hidden px-3 py-2 text-muted md:table-cell">{[d.role, d.platform].filter(Boolean).join(" · ")}</td>
                      <td className="hidden px-3 py-2 font-mono text-xs sm:table-cell">{d.ip}</td>
                      <td className="hidden px-3 py-2 text-muted lg:table-cell">{d.site}</td>
                      <td className="px-3 py-2 text-right whitespace-nowrap">
                        {rejected ? (
                          <Button variant="ghost" disabled={act.isPending} onClick={() => act.mutate({ id: d.id, restore: true })}>
                            {t("common.restore")}
                          </Button>
                        ) : (
                          <>
                            <Button variant="ghost" onClick={() => setAccept(d)}>
                              {t("assets.found.accept")}
                            </Button>
                            <Button variant="ghost" disabled={act.isPending} onClick={() => act.mutate({ id: d.id, restore: false })}>
                              {t("assets.found.reject")}
                            </Button>
                          </>
                        )}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
          <label className="flex items-center gap-2 border-t border-line px-4 py-2 text-sm">
            <input type="checkbox" checked={rejected} onChange={(e) => setRejected(e.target.checked)} />
            {t("assets.found.showRejected")}
          </label>
        </div>
      )}
      {accept && <AcceptExternal item={accept} onClose={() => setAccept(null)} />}
    </Card>
  );
}

function SyncResult({ s }: { s: NetboxState }) {
  const { t, i18n } = useTranslation();
  if (!s.last_sync_at) return <p className="text-xs text-muted">{t("netbox.never")}</p>;
  return (
    <p className={`text-xs ${s.last_error ? "text-bad" : "text-muted"}`} role="status">
      {formatDate(s.last_sync_at, i18n.language)} ·{" "}
      {s.last_error ? s.last_error : t("netbox.result", { fetched: s.fetched, added: s.added, missing: s.missing })}
    </p>
  );
}

/** Настройки NetBox (только суперадминистратор). */
export function NetboxCard() {
  const q = useQuery({ queryKey: ["netbox-admin"], queryFn: () => ok<NetboxView>(api.GET("/api/admin/inventory/netbox")) });
  if (q.error) return <ErrorBox error={q.error} />;
  if (q.isPending) return <Spinner />;
  return <NetboxForm key={q.dataUpdatedAt} data={q.data} />;
}

function NetboxForm({ data }: { data: NetboxView }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [v, setV] = useState(data.settings);
  const [token, setToken] = useState("");
  const [drop, setDrop] = useState(false);
  const save = useMutation({
    meta: { toast: t("common.saved") },
    mutationFn: () => ok(api.PUT("/api/admin/inventory/netbox", { body: { settings: v, token: drop ? "" : token || null } })),
    onSuccess: () => void qc.invalidateQueries({ queryKey: ["netbox-admin"] }),
  });
  const sync = useMutation({
    mutationFn: () => ok<NetboxState>(api.POST("/api/admin/inventory/netbox/sync")),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["netbox-admin"] });
      void qc.invalidateQueries({ queryKey: ["external-inventory"] });
    },
  });
  return (
    <div className="mx-auto max-w-2xl">
      <Card className="grid gap-4 p-5">
        <div>
          <h2 className="font-semibold">{t("netbox.title")}</h2>
          <p className="mt-1 text-xs text-muted">{t("netbox.hint")}</p>
        </div>
        <label className="inline-flex items-center gap-2 text-sm font-medium">
          <input type="checkbox" checked={v.enabled} onChange={(e) => setV({ ...v, enabled: e.target.checked })} />
          {t("netbox.enabled")}
        </label>
        <Field label={t("netbox.url")}>
          <Input value={v.base_url} onChange={(e) => setV({ ...v, base_url: e.target.value })} placeholder="https://netbox.example.local" />
        </Field>
        <Field label={t("netbox.token")} hint={data.has_token ? t("admin.ad.passwordKept") : t("netbox.tokenHint")}>
          <Input type="password" autoComplete="new-password" value={token} disabled={drop} onChange={(e) => setToken(e.target.value)} />
          {data.has_token && (
            <label className="mt-1 inline-flex items-center gap-2 text-xs text-muted">
              <input type="checkbox" checked={drop} onChange={(e) => setDrop(e.target.checked)} />
              {t("netbox.dropToken")}
            </label>
          )}
        </Field>
        <label className="inline-flex items-center gap-2 text-sm">
          <input type="checkbox" checked={v.import_vms} onChange={(e) => setV({ ...v, import_vms: e.target.checked })} />
          {t("netbox.vms")}
        </label>
        <SyncResult s={sync.data ?? data.state} />
        <div className="flex flex-wrap items-center gap-2">
          <Button variant="primary" onClick={() => save.mutate()} disabled={save.isPending}>
            {t("common.save")}
          </Button>
          <Button onClick={() => sync.mutate()} disabled={sync.isPending || !data.settings.base_url || !data.has_token}>
            {sync.isPending ? t("netbox.syncing") : t("netbox.sync")}
          </Button>
        </div>
        {(save.error || sync.error) && <ErrorBox error={save.error ?? sync.error} />}
      </Card>
    </div>
  );
}
