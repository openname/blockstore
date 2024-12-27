(define-data-var transactions map uint tx)

(define-public (store-transaction (tx-id uint) (status (string-ascii 20)) (sender (string-ascii 42)) (recipient (string-ascii 42)) (amount uint))
  (begin
    (map-set transactions tx-id {
      "status": status
      "sender": sender
      "recipient": recipient
      "amount": amount
    })
    (ok "Transaction stored successfully")
  )
)

(define-public (get-transaction-status (tx-id uint))
  (match (map-get? transactions tx-id)
    tx-details
    (ok (get "status" tx-details))
    (err "Transaction not found")
  )
)
