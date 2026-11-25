import { MutationCache, QueryCache, QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { ApiError } from "./api/client";
import "./app.css";
import { Crash } from "./crash";
import "./i18n";
import { applySaved } from "./prefs";
import { registerServiceWorker } from "./push";
import { router } from "./router";
import { toast } from "./toast";

// Тема и плотность из браузера — до первого кадра, без вспышки светлой темы (CSP не пускает встроенный скрипт).
applySaved();

// Сеанс истёк (401 `unauthorized`, не неверный пароль): забыть пользователя — RequireAuth покажет вход,
// а не ошибки на каждом экране. Сам запрос `me` сюда не смотрит, иначе 401 на нём повторялся бы по кругу.
function expired(error: unknown, key?: readonly unknown[]) {
  if (error instanceof ApiError && error.status === 401 && error.code === "unauthorized" && key?.[0] !== "me") {
    void queryClient.resetQueries({ queryKey: ["me"] });
  }
}

const queryClient: QueryClient = new QueryClient({
  defaultOptions: { queries: { retry: 1, staleTime: 10_000 } },
  queryCache: new QueryCache({ onError: (error, query) => expired(error, query.queryKey) }),
  mutationCache: new MutationCache({
    onError: (error) => expired(error),
    // Сообщение об успехе — у мутаций с `meta.toast` (сохранение настроек, действия с заявкой).
    onSuccess: (_data, _vars, _result, mutation) => {
      const text = mutation.meta?.toast;
      if (text) toast(text);
    },
  }),
});

registerServiceWorker();

const root = document.getElementById("root");
if (!root) throw new Error("#root не найден в index.html");

createRoot(root).render(
  <StrictMode>
    <Crash>
      <QueryClientProvider client={queryClient}>
        <RouterProvider router={router} />
      </QueryClientProvider>
    </Crash>
  </StrictMode>,
);
