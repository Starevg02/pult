// Оборудование в новой заявке: сотрудник отмечает своё (он ответственный), инженер — ещё и любое
// из реестра. Кабинет и телефон сюда не относятся — их человек вводит сам.
import { useQuery } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type FoundObject } from "../../api/client";
import { Button, Input } from "../../ui";

/** Поиск по реестру оборудования (только инженеру). */
export function AssetSearch({ onPick, exclude }: { onPick: (a: FoundObject) => void; exclude: string[] }) {
  const { t } = useTranslation();
  const [q, setQ] = useState("");
  const [needle, setNeedle] = useState("");
  useEffect(() => {
    const h = setTimeout(() => setNeedle(q.trim()), 250);
    return () => clearTimeout(h);
  }, [q]);
  const found = useQuery({
    queryKey: ["objects", needle, "asset"],
    queryFn: () => ok<FoundObject[]>(api.GET("/api/helpdesk/objects", { params: { query: { q: needle, kind: "asset" } } })),
    enabled: needle.length > 1,
  });
  const list = (found.data ?? []).filter((a) => !exclude.includes(a.id));
  return (
    <div className="space-y-1">
      <Input autoFocus value={q} onChange={(e) => setQ(e.target.value)} placeholder={t("helpdesk.equipmentSearch")} aria-label={t("helpdesk.equipmentFind")} />
      {found.data && (
        <ul className="max-h-60 divide-y divide-line overflow-auto rounded-lg border border-line">
          {list.length === 0 && <li className="px-3 py-2 text-xs text-muted">{t("card.nothingFound")}</li>}
          {list.map((a) => (
            <li key={a.id}>
              <button type="button" className="w-full px-3 py-2 text-left text-sm hover:bg-hover" onClick={() => onPick(a)}>
                {a.label} {a.detail && <span className="text-xs text-muted">· {a.detail}</span>}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

export function AssetPicker({
  value,
  onChange,
  own,
  canSearch,
  ownLabel,
  requesterKnown = true,
}: {
  value: FoundObject[];
  onChange: (v: FoundObject[]) => void;
  /** Оборудование заявителя: отметить галочкой. */
  own: FoundObject[];
  /** Инженер: можно выбрать любое из реестра. */
  canSearch: boolean;
  ownLabel: string;
  /** Заявка за сотрудника: пока заявитель не выбран, о его оборудовании ничего не пишем. */
  requesterKnown?: boolean;
}) {
  const { t } = useTranslation();
  const [searching, setSearching] = useState(false);
  const picked = new Set(value.map((a) => a.id));
  const toggle = (a: FoundObject) => onChange(picked.has(a.id) ? value.filter((x) => x.id !== a.id) : [...value, a]);
  const extra = value.filter((a) => !own.some((o) => o.id === a.id));
  if (!canSearch && own.length === 0) return null;
  return (
    <fieldset className="space-y-2">
      <legend className="mb-1 text-sm font-medium">{t("helpdesk.equipment")}</legend>
      {own.length > 0 ? (
        <div>
          <div className="mb-1 text-xs text-muted">{ownLabel}</div>
          <ul className="space-y-1">
            {own.map((a) => (
              <li key={a.id}>
                <label className="flex items-start gap-2 text-sm">
                  <input type="checkbox" className="mt-1" checked={picked.has(a.id)} disabled={!picked.has(a.id) && value.length >= 10} onChange={() => toggle(a)} />
                  <span>
                    {a.label} {a.detail && <span className="text-xs text-muted">· {a.detail}</span>}
                  </span>
                </label>
              </li>
            ))}
          </ul>
        </div>
      ) : (
        requesterKnown && <div className="text-xs text-muted">{t("helpdesk.equipmentNone")}</div>
      )}
      {extra.length > 0 && (
        <ul className="space-y-1">
          {extra.map((a) => (
            <li key={a.id} className="flex items-start gap-2 text-sm">
              <span className="min-w-0 flex-1">
                {a.label} {a.detail && <span className="text-xs text-muted">· {a.detail}</span>}
              </span>
              <button type="button" aria-label={t("common.remove")} className="text-xs text-muted hover:text-bad" onClick={() => toggle(a)}>
                ✕
              </button>
            </li>
          ))}
        </ul>
      )}
      {canSearch &&
        (searching ? (
          <div className="space-y-1">
            <AssetSearch
              exclude={value.map((a) => a.id)}
              onPick={(a) => {
                onChange([...value, a]);
                setSearching(false);
              }}
            />
            <Button type="button" variant="ghost" onClick={() => setSearching(false)}>
              {t("common.cancel")}
            </Button>
          </div>
        ) : (
          value.length < 10 && (
            <Button type="button" variant="ghost" onClick={() => setSearching(true)}>
              + {t("helpdesk.equipmentFind")}
            </Button>
          )
        ))}
    </fieldset>
  );
}
