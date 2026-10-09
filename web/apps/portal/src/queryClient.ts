import { QueryClient } from "@tanstack/vue-query";

import { ApiError } from "./api";

/** The one query cache, shared so the session store can empty it when the user changes. */
export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // Don't retry what the server refused on purpose.
      retry: (count, err) => !(err instanceof ApiError && err.status < 500) && count < 2,
      refetchOnWindowFocus: false,
    },
  },
});
