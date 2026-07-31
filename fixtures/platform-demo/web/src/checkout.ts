export async function createOrder(payload: unknown): Promise<Response> {
  return fetch("/api/orders", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(payload),
  });
}
