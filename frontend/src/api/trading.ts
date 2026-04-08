import { del, post, put } from './client';
import type { OrderRequest, OrderResponse } from '../types/order';

export function placeOrder(order: OrderRequest): Promise<OrderResponse> {
  return post<OrderResponse>('/orders', order);
}

export function modifyOrder(
  orderNo: string,
  body: {
    original_krx_orgno: string;
    order_type: string;
    quantity: number;
    price: number;
  },
): Promise<OrderResponse> {
  return put<OrderResponse>(`/orders/${orderNo}/modify`, body);
}

export function cancelOrder(orderNo: string): Promise<OrderResponse> {
  return del<OrderResponse>(`/orders/${orderNo}`);
}
