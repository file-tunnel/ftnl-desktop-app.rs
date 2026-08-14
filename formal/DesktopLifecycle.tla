---- MODULE DesktopLifecycle ----
EXTENDS Naturals, TLC

States == {
    "idle", "creating", "pairing", "refreshing", "transferring",
    "downloading", "cancelling", "complete", "failed_without_session",
    "failed_with_session"
}
OperationIds == 1..3
NoOperation == 0
InFlightStates == {"creating", "refreshing", "downloading", "cancelling"}
SessionStates == {"pairing", "refreshing", "transferring", "downloading", "failed_with_session"}

NeedsSession(candidate) == candidate \in SessionStates
IsInFlight(candidate) == candidate \in InFlightStates
NextId(candidate) == IF candidate = 3 THEN 1 ELSE candidate + 1

VARIABLES state, activeOperation, nextOperation, hasSession, lastResponse, rejected
vars == <<state, activeOperation, nextOperation, hasSession, lastResponse, rejected>>

Init ==
    /\ state = "idle"
    /\ activeOperation = NoOperation
    /\ nextOperation = 1
    /\ hasSession = FALSE
    /\ lastResponse = NoOperation
    /\ rejected = FALSE

BeginTarget ==
    \/ /\ state = "idle"
       /\ state' = "creating"
    \/ /\ state = "complete"
       /\ state' = "creating"
    \/ /\ state = "failed_without_session"
       /\ state' = "creating"
    \/ /\ state \in {"pairing", "transferring", "failed_with_session"}
       /\ state' = IF state = "failed_with_session" THEN "refreshing" ELSE
                      IF state = "pairing" THEN "refreshing" ELSE "downloading"
    \/ /\ state \in {"pairing", "transferring", "failed_with_session"}
       /\ state' = "cancelling"

BeginEffect ==
    /\ BeginTarget
    /\ activeOperation' = nextOperation
    /\ nextOperation' = NextId(nextOperation)
    /\ hasSession' = NeedsSession(state')
    /\ UNCHANGED lastResponse
    /\ rejected' = FALSE

RefreshFromTransfer ==
    /\ state = "transferring"
    /\ state' = "refreshing"
    /\ activeOperation' = nextOperation
    /\ nextOperation' = NextId(nextOperation)
    /\ hasSession' = TRUE
    /\ UNCHANGED lastResponse
    /\ rejected' = FALSE

Reset ==
    /\ state \in {"complete", "failed_without_session", "failed_with_session"}
    /\ state' = "idle"
    /\ activeOperation' = NoOperation
    /\ hasSession' = FALSE
    /\ UNCHANGED <<nextOperation, lastResponse>>
    /\ rejected' = FALSE

CompletionTarget ==
    \/ /\ state = "creating"
       /\ state' \in {"pairing", "failed_without_session"}
    \/ /\ state = "refreshing"
       /\ state' \in {"pairing", "transferring", "complete", "failed_with_session"}
    \/ /\ state = "downloading"
       /\ state' \in {"transferring", "failed_with_session"}
    \/ /\ state = "cancelling"
       /\ state' \in {"idle", "failed_without_session"}

CompleteEffect ==
    /\ activeOperation \in OperationIds
    /\ CompletionTarget
    /\ activeOperation' = NoOperation
    /\ hasSession' = NeedsSession(state')
    /\ lastResponse' = activeOperation
    /\ UNCHANGED nextOperation
    /\ rejected' = FALSE

StaleResponse ==
    \E response \in OperationIds:
        /\ response # activeOperation
        /\ UNCHANGED <<state, activeOperation, nextOperation, hasSession>>
        /\ lastResponse' = response
        /\ rejected' = TRUE

Next == BeginEffect \/ RefreshFromTransfer \/ Reset \/ CompleteEffect \/ StaleResponse
Spec == Init /\ [][Next]_vars

TypeInvariant ==
    /\ state \in States
    /\ activeOperation \in OperationIds \cup {NoOperation}
    /\ nextOperation \in OperationIds
    /\ hasSession \in BOOLEAN
    /\ lastResponse \in OperationIds \cup {NoOperation}
    /\ rejected \in BOOLEAN

SessionAuthorityInvariant == hasSession = NeedsSession(state)
SingleInFlightInvariant == (activeOperation # NoOperation) = IsInFlight(state)
StaleResponseInvariant == rejected => lastResponse # activeOperation
TerminalInvariant == state = "complete" => ~hasSession /\ activeOperation = NoOperation

====
