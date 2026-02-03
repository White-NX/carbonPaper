import { clsx } from "clsx"
import { twMerge } from "tailwind-merge"

export const DEFAULT_API_URL = 'http://127.0.0.1:8188';

export function getApiBase() {
  if (typeof window !== 'undefined') {
    return localStorage.getItem('COMFY_API_URL') || DEFAULT_API_URL;
  }
  return DEFAULT_API_URL;
}

export function cn(...inputs) {
  return twMerge(clsx(inputs))
}
