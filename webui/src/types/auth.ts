// Auth types — aligned with peco-server /api/auth/*
export interface User {
  id: string
  username: string
  email: string
  avatar?: string
  created_at: string
}

export interface AuthResponse {
  user: User
  token: string
}

export interface LoginRequest {
  email: string
  password: string
}

export interface RegisterRequest {
  username: string
  email: string
  password: string
}
