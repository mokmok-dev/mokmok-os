import { useQuery } from "@tanstack/react-query";

import { parseHealthResponse, type HealthResponse } from "@mokmok/web-protocol";

const fetchHealth = async (): Promise<HealthResponse> => {
  const response = await fetch("/api/health", { headers: { accept: "application/json" } });
  if (!response.ok) throw new Error(`The health endpoint answered ${response.status}`);
  const parsed = parseHealthResponse(await response.text());
  if (parsed === null) throw new Error("The health endpoint returned an unexpected document");
  return parsed;
};

export const useHealth = () =>
  useQuery({
    queryKey: ["bridge-health"],
    queryFn: fetchHealth,
    refetchInterval: 3_000,
    staleTime: 1_000,
    retry: false,
  });
