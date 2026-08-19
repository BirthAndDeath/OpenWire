// 文件分享消息元信息（前端共享类型）
export interface FileHashInfo {
  filename: string;
  total_size: number;
  file_hash: string; // hex
}