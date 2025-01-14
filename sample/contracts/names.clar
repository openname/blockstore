(define-constant burn-address 'SP000000000000000000002Q6VF78)
(define-private (price-function (name uint))
  (if (< name u100000) u1000 u100))
         
(define-map name-map 
  { name: uint } { owner: principal })
(define-map preorder-map
  { name-hash: (buff 20) }
  { buyer: principal, paid: uint })
         
(define-public (preorder 
                (name-hash (buff 20))
                (name-price uint))
  (if (is-ok (contract-call? .tokens token-transfer
                burn-address name-price))
      (begin (map-insert preorder-map
                     (tuple (name-hash name-hash))
                     (tuple (paid name-price)
                            (buyer tx-sender)))
             (ok u0))
      (err "token payment failed.")))

(define-public (register 
                (recipient-principal principal)
                (name uint)
                (salt uint))
  (let ((preorder-entry
         (unwrap! ;; name _must_ have been preordered.
           (map-get? preorder-map
             (tuple (name-hash (hash160 (xor name salt)))))
           (err "no preorder found")))
        (name-entry 
         (map-get? name-map (tuple (name name)))))
    (if (and
         ;; name shouldn't *already* exist
         (is-none name-entry)
         ;; preorder must have paid enough
         (<= (price-function name) 
             (get paid preorder-entry))
         ;; preorder must have been the current principal
         (is-eq tx-sender
              (get buyer preorder-entry)))
        (if (and
              (map-insert name-map
                        (tuple (name name))
                        (tuple (owner recipient-principal)))
              (map-delete preorder-map
                        (tuple (name-hash (hash160 (xor name salt))))))
            (ok u0)
            (err "failed to insert new name entry"))
        (err "invalid name register"))))
