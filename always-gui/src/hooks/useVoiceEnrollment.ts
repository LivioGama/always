import { useState, useCallback, useRef } from "react";
import { ENROLLMENT_STEPS } from "../types/uds";

export interface EnrollmentState {
  currentStep: number;
  isEnrolling: boolean;
  isRecording: boolean;
  energy: number;
  progress: { voiced_ms: number; target_ms: number };
  error: string | null;
}

export function useVoiceEnrollment() {
  const [state, setState] = useState<EnrollmentState>({
    currentStep: 0,
    isEnrolling: false,
    isRecording: false,
    energy: 0,
    progress: { voiced_ms: 0, target_ms: 0 },
    error: null,
  });

  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const startEnrollment = useCallback((stepIndex: number) => {
    setState((prev) => ({
      ...prev,
      currentStep: stepIndex,
      isEnrolling: true,
      isRecording: true,
      energy: 0,
      progress: { voiced_ms: 0, target_ms: 15000 },
      error: null,
    }));
  }, []);

  const stopEnrollment = useCallback((success: boolean, error?: string) => {
    if (timerRef.current) clearInterval(timerRef.current);
    timerRef.current = null;

    setState((prev) => {
      if (success) {
        const nextStep = Math.min(prev.currentStep + 1, ENROLLMENT_STEPS.length - 1);
        return {
          ...prev,
          isRecording: false,
          isEnrolling: nextStep < ENROLLMENT_STEPS.length - 1,
          currentStep: nextStep,
        };
      }
      return {
        ...prev,
        isRecording: false,
        isEnrolling: false,
        error: error ?? "Enrollment failed",
      };
    });
  }, []);

  const cancelEnrollment = useCallback(() => {
    if (timerRef.current) clearInterval(timerRef.current);
    timerRef.current = null;
    setState((prev) => ({
      ...prev,
      isRecording: false,
      isEnrolling: false,
      error: null,
    }));
  }, []);

  const updateProgress = useCallback(
    (voiced_ms: number, target_ms: number, energy: number) => {
      setState((prev) => ({
        ...prev,
        energy,
        progress: { voiced_ms, target_ms },
      }));
    },
    [],
  );

  // Cleanup on unmount
  if (typeof window !== "undefined") {
    // eslint-disable-next-line react-hooks/rules-of-hooks
    const cleanupRef = useRef(true);
    if (cleanupRef.current) {
      cleanupRef.current = false;
      // Cleanup handled by caller via state
    }
  }

  return {
    state,
    currentStep: ENROLLMENT_STEPS[state.currentStep] ?? null,
    totalSteps: ENROLLMENT_STEPS.length,
    steps: ENROLLMENT_STEPS,
    startEnrollment,
    stopEnrollment,
    cancelEnrollment,
    updateProgress,
  };
}