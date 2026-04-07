export interface VideoResponse {
  token: string;
  original_name: string;
  content_type: string;
  size_bytes: number;
  duration_secs: number | null;
  hls_ready: boolean;
  created_at: string;
  stream_url: string;
  share_url: string;
}

export interface UploadResponse {
  token: string;
  share_url: string;
  stream_url: string;
  original_name: string;
  size_bytes: number;
}
