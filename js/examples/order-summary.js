order => ({
  customer: order.customer,
  item_count: String(order.items.reduce((count, item) => count + item.quantity, 0)),
  total_cents: String(order.items.reduce((total, item) => total + item.quantity * item.price_cents, 0))
})
