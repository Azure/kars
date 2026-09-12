import type { Dispatch, SetStateAction } from "react";
import type {
  Blueprint,
  BlueprintEgress,
  Efficiency,
  ExecutionPlan,
  MissionDelegation,
  Options,
  ValidationResult,
} from "@/lib/types";
import type { IntakeState } from "../actions";

type Setter<T> = Dispatch<SetStateAction<T>>;

export interface ReviewProps {
  formAction: (payload: FormData) => void;
  objective: string;
  tier: number;
  budgetTokens: string;
  launch: boolean;
  blueprint: Blueprint;
  delegation: MissionDelegation;
  rationale: string | null;
  proposed: boolean;
  composeSource: string | null;
  recommendedRoute: string | null;
  modelBasis: string | null;
  composeNote: string | null;
  setObjective: Setter<string>;
  options: Options;
  model: string;
  setModel: Setter<string>;
  setModelFallbacks: Setter<string[]>;
  modelFallbacks: string[];
  recommendedModel: Options["models"][number] | null;
  efficiency: Efficiency | null | undefined;
  recommendedStats: Efficiency["routes"][number] | null;
  runtime: string;
  changeRuntime: (nextRuntime: string) => void;
  instructions: string;
  setInstructions: Setter<string>;
  executionPlanDraft: string;
  setExecutionPlanDraft: Setter<string>;
  setExecutionPlan: Setter<ExecutionPlan | null>;
  setExecutionPlanError: Setter<string | null>;
  executionPlanError: string | null;
  loopDirective: string;
  cameViaLoopReview: boolean;
  goBackToLoopReview: () => void;
  toolPolicy: string;
  setToolPolicy: Setter<string>;
  mcp: string[];
  setMcp: Setter<string[]>;
  mcpNeedsPolicy: boolean;
  skills: string[];
  setSkills: Setter<string[]>;
  setEgressMode: Setter<"strict" | "learning">;
  egressMode: "strict" | "learning";
  egress: BlueprintEgress[];
  setEgress: Setter<BlueprintEgress[]>;
  isolation: string;
  setIsolation: Setter<string>;
  memory: string;
  setMemory: Setter<string>;
  setTier: Setter<number>;
  ceiling: number;
  setBudgetTokens: Setter<string>;
  runValidation: () => Promise<void>;
  validating: boolean;
  validationError: string | null;
  validation: ValidationResult | null;
  validationFresh: boolean;
  setLaunch: Setter<boolean>;
  state: IntakeState;
  goBack: () => void;
}
