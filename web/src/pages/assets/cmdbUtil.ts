// Состояния оборудования и жизненный цикл типа.
import type { Lifecycle } from "../../api/client";

export const ASSET_STATUSES = ["in_use", "in_stock", "repair", "retired"] as const;

/** Состояния типа по его жизненному циклу; без данных — все. */
export function statusesOf(type: { lifecycle?: Lifecycle } | undefined): string[] {
  return type?.lifecycle?.statuses ?? [...ASSET_STATUSES];
}
