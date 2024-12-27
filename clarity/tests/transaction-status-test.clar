(use-contract transaction-status)

;; Test constants
(define-constant tx-id 1)
(define-constant status "Pending")
(define-constant sender "SP3FBR2AGKZ5AA93A3DQVJ8K2KD7DZ4Y0E61J2H1A")
(define-constant recipient "SP3FBR2AGKZ5AA93A3DQVJ8K2KD7DZ4Y0E61J2H1B")
(define-constant amount u100)

(begin
  ;; Test storing a transaction
  (let ((store-result (transaction-status::store-transaction tx-id status sender recipient amount)))
    (asserts! (is-eq store-result (ok "Transaction stored successfully")) "Failed to store transaction"))

  ;; Test retrieving the transaction's status
  (let ((query-result (transaction-status::get-transaction-status tx-id)))
    (asserts! (is-eq query-result (ok status)) "Failed to retrieve transaction status"))

  ;; Test querying a non-existent transaction
  (let ((query-missing (transaction-status::get-transaction-status u999)))
    (asserts! (is-err query-missing) "Expected an error for missing transaction"))
)
