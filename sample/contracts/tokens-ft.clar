(define-fungible-token tokens)
(define-private (get-balance (account principal))
  (ft-get-balance tokens account))

(define-private (token-credit! (account principal) (amount uint))
  (ft-mint? tokens amount account))

(define-public (token-transfer (to principal) (amount uint))
  (ft-transfer? tokens amount tx-sender to))

(define-public (mint! (amount uint))
  (token-credit! tx-sender amount))

(token-credit! tx-sender u10300)
(token-transfer 'SZ2J6ZY48GV1EZ5V2V5RB9MP66SW86PYKKQ9H6DPR u10000)
(token-transfer 'SM2J6ZY48GV1EZ5V2V5RB9MP66SW86PYKKQVX8X0G u300)
