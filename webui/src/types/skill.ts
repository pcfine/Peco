// Skill types — aligned with peco-server /api/skills/*

export interface SkillListItem {
  name: string;
  description: string;
}

export interface SkillDetail {
  name: string;
  content: string;
}
